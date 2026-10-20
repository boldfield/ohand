//! Outbound destination and credential isolation. See `main.rs` for scope.

use crate::fixtures::*;
use chrono::Duration;
use ohand_core::jobs::configuration::revoke_profile;
use ohand_core::jobs::runner::{JobResult, Settlement};
use ohand_core::privacy::routing::{
    authorize_job, grant_route_capability, list_route_policy, revoke_route_capability,
    AuthorizationDecision, DenialReason, DestinationClass, ListedDestinationClass,
    ProcessingCapability,
};
use ohand_core::providers::anthropic::{HttpRequest, CREDENTIAL_HEADER};
use ohand_core::providers::contracts::ProviderProfile;
use serde_json::json;

fn grant(world: &World, profile: &ProviderProfile) {
    grant_route_capability(
        &mut world.database(),
        PERSONAL_ROUTE,
        ProcessingCapability::TextInterpretation,
        profile.profile_version(),
        "2026-10-08T09:30:03Z",
    )
    .unwrap();
}

fn decision(world: &World, job_id: &str) -> AuthorizationDecision {
    authorize_job(world.database().conn(), job_id).unwrap()
}

fn assert_nothing_sent(providers: &Providers) {
    assert!(
        providers.anthropic_requests().is_empty() && providers.private_requests().is_empty(),
        "no transport call may happen"
    );
}

fn assert_only_sent_to(providers: &Providers, anthropic_prefix: &str, private_prefix: &str) {
    for request in providers.anthropic_requests() {
        assert!(request.url.starts_with(anthropic_prefix), "{}", request.url);
    }
    for request in providers.private_requests() {
        assert!(
            request.endpoint.starts_with(private_prefix),
            "{}",
            request.endpoint
        );
    }
}

fn debug_of(request: &HttpRequest) -> String {
    format!("{request:?}")
}

// ---- Unapproved capability or destination ----

#[test]
fn an_unapproved_capability_or_destination_sends_no_canary() {
    type Policy = (
        &'static str,
        Vec<&'static str>,
        Option<(&'static str, Vec<&'static str>)>,
        DenialReason,
    );
    let scenarios: Vec<Policy> = vec![
        (
            "fresh install: local-only route",
            vec![],
            None,
            DenialReason::DestinationNotInRoute,
        ),
        (
            "destination is in the ceiling but no capability was granted",
            vec![ANTHROPIC_ORIGIN],
            None,
            DenialReason::CapabilityNotAuthorized,
        ),
        (
            "only review was granted, which never implies interpretation",
            vec![ANTHROPIC_ORIGIN],
            Some(("review", vec![ANTHROPIC_ORIGIN])),
            DenialReason::CapabilityNotAuthorized,
        ),
        (
            "the route and the grant name another destination",
            vec![OTHER_ORIGIN],
            Some(("text_interpretation", vec![OTHER_ORIGIN])),
            DenialReason::DestinationNotInRoute,
        ),
        (
            "the ceiling allows it but the capability grant names another destination",
            vec![ANTHROPIC_ORIGIN, OTHER_ORIGIN],
            Some(("text_interpretation", vec![OTHER_ORIGIN])),
            DenialReason::DestinationNotAuthorizedForCapability,
        ),
    ];
    for (name, ceiling, grant_row, expected) in scenarios {
        let world = World::new();
        let profile = anthropic_profile();
        world.install_profile(&profile);
        world.set_policy(
            &ceiling,
            grant_row.as_ref().map(|(c, d)| (*c, d.as_slice())),
        );
        let job_id = world.enqueue(Some(&profile));
        assert_eq!(decision(&world, &job_id).denial(), Some(expected), "{name}");

        let providers = Providers::new(vec![anthropic_reply(annotate_action_proposal())], vec![]);
        let clock = SteppedClock::after_setup();
        for _ in 0..3 {
            world.drain(&providers, &clock);
            clock.advance(Duration::hours(2));
        }
        assert_nothing_sent(&providers);
        let row = world.job_row(&job_id);
        assert_eq!(row.status, "queued", "{name}: waits, never dispatches");
        assert_eq!(
            row.failure_reason.as_deref(),
            Some("unauthorized"),
            "{name}"
        );
        assert_eq!(world.count("SELECT COUNT(*) FROM proposals"), 0, "{name}");
    }
}

#[test]
fn a_private_server_profile_needs_its_own_destination_grant() {
    let world = World::new();
    let profile = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    world.install_profile(&profile);
    // The cloud origin is granted; the private server is not.
    world.set_policy(
        &[ANTHROPIC_ORIGIN],
        Some(("text_interpretation", &[ANTHROPIC_ORIGIN])),
    );
    let job_id = world.enqueue(Some(&profile));
    assert_eq!(
        decision(&world, &job_id).denial(),
        Some(DenialReason::DestinationNotInRoute)
    );
    let providers = Providers::new(vec![], vec![private_reply(&annotate_action_proposal())]);
    world.drain(&providers, &SteppedClock::after_setup());
    assert_nothing_sent(&providers);
}

#[test]
fn an_adapter_endpoint_outside_the_profiles_destinations_sends_nothing() {
    let lookalikes = [
        "https://api.anthropic.com.evil.example/v1/messages",
        "https://api.anthropic.com@evil.example/v1/messages",
        "http://api.anthropic.com/v1/messages",
        "https://api.anthropic.com:8443/v1/messages",
        "https://evil.example/v1/messages",
    ];
    for endpoint in lookalikes {
        let world = World::new();
        let profile = anthropic_profile();
        world.install_profile(&profile);
        grant(&world, &profile);
        let job_id = world.enqueue(Some(&profile));
        // Route authorization passes; the adapter's own destination check must still hold.
        assert!(decision(&world, &job_id).authorization().is_some());
        let providers = Providers::with_anthropic_endpoint(
            vec![anthropic_reply(annotate_action_proposal())],
            vec![],
            Some(endpoint),
        );
        world.drain(&providers, &SteppedClock::after_setup());
        assert_nothing_sent(&providers);
        assert_eq!(
            world.count("SELECT COUNT(*) FROM proposals"),
            0,
            "{endpoint}"
        );
    }
}

// ---- Redirects ----

#[test]
fn a_cloud_redirect_is_not_followed_and_leaks_nothing() {
    let world = World::new();
    let profile = anthropic_profile();
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(Some(&profile));
    let providers = Providers::new(
        vec![
            anthropic_redirect(),
            anthropic_redirect(),
            anthropic_redirect(),
        ],
        vec![],
    );
    let clock = SteppedClock::after_setup();
    for _ in 0..3 {
        world.drain(&providers, &clock);
        clock.advance(Duration::hours(2));
    }
    let requests = providers.anthropic_requests();
    assert!(!requests.is_empty());
    // Core only ever addressed the authorized origin: a redirect response is a failed attempt,
    // never a reason to send the body somewhere else.
    assert_only_sent_to(&providers, ANTHROPIC_ORIGIN, PRIVATE_ORIGIN_A);
    assert!(requests
        .iter()
        .all(|request| !request.url.contains("redirect-target")));
    assert_eq!(world.count("SELECT COUNT(*) FROM proposals"), 0);
    assert_ne!(world.job_row(&job_id).status, "completed");
    assert!(!world.dump_state().contains("redirect-target"));
}

#[test]
fn a_private_server_redirect_is_not_followed_and_keeps_the_job_queued() {
    let world = World::new();
    let profile = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(Some(&profile));
    let providers = Providers::new(
        vec![],
        vec![private_redirect(), private_redirect(), private_redirect()],
    );
    let clock = SteppedClock::after_setup();
    for _ in 0..3 {
        world.drain(&providers, &clock);
        clock.advance(Duration::hours(2));
    }
    assert!(!providers.private_requests().is_empty());
    assert_only_sent_to(&providers, ANTHROPIC_ORIGIN, PRIVATE_ORIGIN_A);
    assert_eq!(world.count("SELECT COUNT(*) FROM proposals"), 0);
    assert_ne!(world.job_row(&job_id).status, "completed");
    assert!(!world.dump_state().contains("redirect-target"));
}

// ---- Approved remote processing ----

#[test]
fn approved_cloud_processing_is_identified_and_reaches_only_its_destination() {
    let world = World::new();
    let profile = anthropic_profile();
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(Some(&profile));

    let decision = decision(&world, &job_id);
    let authorization = decision.authorization().expect("approved");
    assert!(!authorization.is_local());
    assert_eq!(
        authorization.destination_class(),
        Some(DestinationClass::Cloud)
    );
    assert_eq!(authorization.destinations(), [ANTHROPIC_ORIGIN.to_string()]);
    assert_eq!(
        authorization.profile_version(),
        Some(profile.profile_version())
    );
    let policy = list_route_policy(world.database().conn()).unwrap();
    let capabilities = &policy[0].granted_capabilities;
    assert_eq!(capabilities.len(), 1);
    assert_eq!(
        capabilities[0].destination_classes,
        vec![ListedDestinationClass::Cloud]
    );

    let providers = Providers::new(vec![anthropic_reply(annotate_action_proposal())], vec![]);
    world.drain(&providers, &SteppedClock::after_setup());
    let requests = providers.anthropic_requests();
    assert_eq!(requests.len(), 1);
    assert_only_sent_to(&providers, ANTHROPIC_ORIGIN, PRIVATE_ORIGIN_A);
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(
        body.contains(CONTENT_CANARY),
        "approved processing sends the text"
    );
    assert_eq!(world.job_row(&job_id).status, "completed");
}

#[test]
fn approved_private_server_processing_is_identified_as_a_private_server() {
    let world = World::new();
    let profile = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(Some(&profile));

    let decision = decision(&world, &job_id);
    let authorization = decision.authorization().expect("approved");
    assert!(!authorization.is_local());
    assert_eq!(
        authorization.destination_class(),
        Some(DestinationClass::PrivateServer)
    );
    assert_eq!(authorization.destinations(), [PRIVATE_ORIGIN_A.to_string()]);
    let policy = list_route_policy(world.database().conn()).unwrap();
    assert_eq!(
        policy[0].granted_capabilities[0].destination_classes,
        vec![ListedDestinationClass::PrivateServer]
    );

    let providers = Providers::new(vec![], vec![private_reply(&annotate_action_proposal())]);
    world.drain(&providers, &SteppedClock::after_setup());
    let requests = providers.private_requests();
    assert_eq!(requests.len(), 1);
    assert_only_sent_to(&providers, ANTHROPIC_ORIGIN, PRIVATE_ORIGIN_A);
    assert!(requests[0].body.contains(CONTENT_CANARY));
}

#[test]
fn a_job_without_a_profile_is_identified_as_local_and_sends_nothing() {
    let world = World::new();
    // Even with a remote profile installed and granted, an unpinned job never uses it.
    let profile = anthropic_profile();
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(None);

    let decision = decision(&world, &job_id);
    let authorization = decision
        .authorization()
        .expect("local processing needs no grant");
    assert!(authorization.is_local());
    assert_eq!(authorization.destination_class(), None);
    assert!(authorization.destinations().is_empty());

    let providers = Providers::new(
        vec![anthropic_reply(annotate_action_proposal())],
        vec![private_reply(&annotate_action_proposal())],
    );
    world.drain(&providers, &SteppedClock::after_setup());
    assert_nothing_sent(&providers);
}

// ---- Credentials ----

#[test]
fn credentials_travel_as_references_and_never_in_headers_or_bodies() {
    let world = World::new();
    let cloud = anthropic_profile();
    let private = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    for profile in [&cloud, &private] {
        world.install_profile(profile);
        grant(&world, profile);
    }
    world.enqueue(Some(&cloud));
    world.enqueue(Some(&private));
    let providers = Providers::new(
        vec![anthropic_reply(annotate_action_proposal())],
        vec![private_reply(&annotate_action_proposal())],
    );
    world.drain(&providers, &SteppedClock::after_setup());

    let route_secrets = [
        PERSONAL_ROUTE,
        ANTHROPIC_CREDENTIAL_REF,
        PRIVATE_CREDENTIAL_REF,
    ];
    let cloud_requests = providers.anthropic_requests();
    assert_eq!(cloud_requests.len(), 1);
    let cloud_request = &cloud_requests[0];
    let attachment = cloud_request
        .credential
        .as_ref()
        .expect("credential reference");
    assert_eq!(attachment.reference, ANTHROPIC_CREDENTIAL_REF);
    assert_eq!(attachment.header, CREDENTIAL_HEADER);
    assert!(
        cloud_request.headers.iter().all(|(name, value)| !name
            .eq_ignore_ascii_case(CREDENTIAL_HEADER)
            && !value.contains("credential-ref")),
        "core never fills the credential header"
    );
    let cloud_body = String::from_utf8(cloud_request.body.clone()).unwrap();
    for secret in route_secrets {
        assert!(
            !cloud_body.contains(secret),
            "{secret} reached the cloud body"
        );
    }
    assert!(!cloud_body.contains(&world.item_id));

    let private_requests = providers.private_requests();
    assert_eq!(private_requests.len(), 1);
    let private_request = &private_requests[0];
    assert_eq!(private_request.credential_ref, PRIVATE_CREDENTIAL_REF);
    assert!(private_request.headers.iter().all(|(name, value)| {
        !name.eq_ignore_ascii_case("authorization") && !value.contains("credential-ref")
    }));
    for secret in route_secrets {
        assert!(
            !private_request.body.contains(secret),
            "{secret} reached the private body"
        );
    }
    assert!(!private_request.body.contains(&world.item_id));
}

// ---- Stale, revoked and in-flight configuration ----

#[test]
fn a_revoked_profile_retires_the_queued_job_and_is_never_repinned() {
    let world = World::new();
    let old = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    let replacement = private_profile("synthetic-private-next", PRIVATE_ORIGIN_B);
    for profile in [&old, &replacement] {
        world.install_profile(profile);
        grant(&world, profile);
    }
    let job_id = world.enqueue(Some(&old));
    revoke_profile(&mut world.database(), old.profile_version(), setup_time()).unwrap();

    assert_eq!(
        decision(&world, &job_id).denial(),
        Some(DenialReason::ProfileRevoked)
    );
    let providers = Providers::new(
        vec![],
        vec![
            private_reply(&annotate_action_proposal()),
            private_reply(&annotate_action_proposal()),
        ],
    );
    let clock = SteppedClock::after_setup();
    world.drain(&providers, &clock);
    clock.advance(Duration::days(1));
    world.drain(&providers, &clock);
    assert_nothing_sent(&providers);
    let row = world.job_row(&job_id);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.profile_version.as_deref(), Some(old.profile_version()));
    assert_eq!(
        world.count(&format!(
            "SELECT COUNT(*) FROM jobs WHERE profile_version = '{}'",
            replacement.profile_version()
        )),
        0,
        "the replacement version received no silently moved job"
    );
}

#[test]
fn a_queued_job_stays_pinned_to_its_destination_when_a_newer_version_exists() {
    let world = World::new();
    let pinned = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    let newer = private_profile("synthetic-private", PRIVATE_ORIGIN_B);
    for profile in [&pinned, &newer] {
        world.install_profile(profile);
        grant(&world, profile);
    }
    let job_id = world.enqueue(Some(&pinned));
    let providers = Providers::new(vec![], vec![private_reply(&annotate_action_proposal())]);
    world.drain(&providers, &SteppedClock::after_setup());
    let requests = providers.private_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].endpoint.starts_with(PRIVATE_ORIGIN_A));
    assert!(!requests[0].endpoint.contains("private-b"));
    assert_eq!(
        world.job_row(&job_id).profile_version.as_deref(),
        Some(pinned.profile_version())
    );
}

#[test]
fn a_route_grant_revoked_before_dispatch_sends_nothing() {
    let world = World::new();
    let profile = anthropic_profile();
    world.install_profile(&profile);
    grant(&world, &profile);
    world.enqueue(Some(&profile));
    revoke_route_capability(
        &mut world.database(),
        PERSONAL_ROUTE,
        ProcessingCapability::TextInterpretation,
    )
    .unwrap();
    let providers = Providers::new(vec![anthropic_reply(annotate_action_proposal())], vec![]);
    let clock = SteppedClock::after_setup();
    world.drain(&providers, &clock);
    clock.advance(Duration::hours(2));
    world.drain(&providers, &clock);
    assert_nothing_sent(&providers);
}

#[test]
fn a_grant_revoked_in_flight_discards_the_result_and_sends_no_second_request() {
    let world = World::new();
    let profile = anthropic_profile();
    world.install_profile(&profile);
    grant(&world, &profile);
    let job_id = world.enqueue(Some(&profile));
    let providers = Providers::new(
        vec![
            anthropic_reply(annotate_action_proposal()),
            anthropic_reply(annotate_action_proposal()),
        ],
        vec![],
    );
    let database = world.database();
    providers.during_next_anthropic_call(move || {
        let mut database = database;
        revoke_route_capability(
            &mut database,
            PERSONAL_ROUTE,
            ProcessingCapability::TextInterpretation,
        )
        .unwrap();
    });
    let clock = SteppedClock::after_setup();
    world.drain(&providers, &clock);
    assert_eq!(
        providers.anthropic_requests().len(),
        1,
        "the call was approved when it started"
    );
    assert_eq!(
        world.count("SELECT COUNT(*) FROM proposals"),
        0,
        "result discarded"
    );
    assert_ne!(world.job_row(&job_id).status, "completed");

    for _ in 0..3 {
        clock.advance(Duration::hours(2));
        let report = world.drain(&providers, &clock);
        assert!(report
            .jobs
            .iter()
            .all(|job| job.result != JobResult::Settled(Settlement::Completed)));
    }
    assert_eq!(
        providers.anthropic_requests().len(),
        1,
        "nothing more is sent once the grant is gone"
    );
}

#[test]
fn a_profile_revoked_in_flight_sends_no_second_request() {
    let world = World::new();
    let profile = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    world.install_profile(&profile);
    grant(&world, &profile);
    world.enqueue(Some(&profile));
    let providers = Providers::new(
        vec![],
        vec![
            private_reply(&annotate_action_proposal()),
            private_reply(&annotate_action_proposal()),
        ],
    );
    let database = world.database();
    let profile_version = profile.profile_version().to_string();
    providers.during_next_private_call(move || {
        let mut database = database;
        revoke_profile(&mut database, &profile_version, setup_time()).unwrap();
    });
    let clock = SteppedClock::after_setup();
    world.drain(&providers, &clock);
    for _ in 0..3 {
        clock.advance(Duration::hours(2));
        world.drain(&providers, &clock);
    }
    assert_eq!(providers.private_requests().len(), 1);
    assert_eq!(world.count("SELECT COUNT(*) FROM proposals"), 0);
}

// ---- Logs and stored diagnostics ----

#[test]
fn provider_error_text_and_transport_debug_output_leak_no_secret_or_content() {
    let world = World::new();
    let cloud = anthropic_profile();
    let private = private_profile("synthetic-private", PRIVATE_ORIGIN_A);
    for profile in [&cloud, &private] {
        world.install_profile(profile);
        grant(&world, profile);
    }
    world.enqueue(Some(&cloud));
    world.enqueue(Some(&private));
    let providers = Providers::new(vec![anthropic_auth_failure()], vec![private_auth_failure()]);
    let report = world.drain(&providers, &SteppedClock::after_setup());
    assert_eq!(report.jobs.len(), 2);
    assert_eq!(providers.call_count(), 2);

    let mut diagnostics = vec![format!("{report:?}")];
    for request in providers.anthropic_requests() {
        diagnostics.push(debug_of(&request));
    }
    for diagnostic in &diagnostics {
        for forbidden in [
            SECRET_CANARY,
            CONTENT_CANARY,
            ANTHROPIC_CREDENTIAL_REF,
            PRIVATE_CREDENTIAL_REF,
        ] {
            assert!(
                !diagnostic.contains(forbidden),
                "{forbidden} appeared in diagnostics: {diagnostic}"
            );
        }
    }
    let state = world.dump_state();
    assert!(
        !state.contains(SECRET_CANARY),
        "a provider-side secret must never reach durable state"
    );
    assert_eq!(
        json!(world.count("SELECT COUNT(*) FROM proposals")),
        json!(0)
    );
}

#[test]
fn a_denied_job_records_no_content_and_no_destination_details() {
    let world = World::new();
    let profile = anthropic_profile();
    world.install_profile(&profile);
    let job_id = world.enqueue(Some(&profile));
    let providers = Providers::new(vec![], vec![]);
    let report = world.drain(&providers, &SteppedClock::after_setup());
    assert_nothing_sent(&providers);
    let row = world.job_row(&job_id);
    let recorded = format!("{row:?} {report:?}");
    assert!(!recorded.contains(CONTENT_CANARY));
    assert!(!recorded.contains(ANTHROPIC_CREDENTIAL_REF));
}
