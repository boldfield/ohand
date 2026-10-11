//! Command-line interface for the developer-only paired model comparison.
//!
//! `bench plan` is the dry run and `bench run` executes; both compose the corpus loader, the two
//! arm profiles, the destination authorizer, the bounded resumable executor and (in live mode)
//! the host transport. Nothing here is linked into the app.
//!
//! Safety properties this module owns:
//! - Exactly two profiles and one explicitly named synthetic corpus are required. The command
//!   never discovers, opens or accepts production data, captures or stores.
//! - `help` and `plan` read no credential and open no socket. `plan` writes nothing.
//! - Network use needs `--mode live` and `--execute-network`. Each hosted destination also needs
//!   its own `--allow-destination`, and each credential reference its own `--credential-env`:
//!   selecting a profile never grants a destination or a secret.
//! - Every refusal, including a rerun naming a side that was never dispatched, happens before the
//!   journal is created or written or any request is reserved.
//! - Results are diagnostic: the executor holds no store or apply handle, and the mode, corpus,
//!   instruction, profile and build revisions are pinned in the journal's experiment record.

mod args;
mod authority;
mod corpus;
mod interrupt;
mod output;
mod profiles;
#[cfg(test)]
mod tests;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use ohand_core::providers::contracts::ProviderAdapter;
use ohand_core::providers::contracts::{CancelToken, DispatchLimits, SystemClock};

use crate::executor::{
    ArmBinding, Executor, ExecutorLimits, RunControls, RunRequest, MAX_REQUEST_CAP,
};
use crate::records::Experiment;
use crate::transport::{
    CredentialResolver, DestinationPolicy, EnvCredentialResolver, HostTransport,
};

use args::{Command, Mode, Options};
use authority::ScratchAuthority;
use output::{sanitize_to, PlanView, PriorJournal, ReportView};
use profiles::{build_profile, live_adapter, ArmProfile, EndpointOverrides, SyntheticAdapter};

pub use args::USAGE;
pub use interrupt::cancel_on_interrupt;

const MESSAGE_LIMIT: usize = 400;

/// Exit status of a finished command.
pub const EXIT_COMPLETED: i32 = 0;
pub const EXIT_INCOMPLETE: i32 = 1;
pub const EXIT_REFUSED: i32 = 2;

/// Looks up one environment variable by name.
pub type EnvironmentLookup = Arc<dyn Fn(&OsStr) -> Option<OsString> + Send + Sync>;

/// Everything the command takes from outside the process. The binary passes the real
/// environment; tests pass a scripted one and, for fixture servers, an endpoint and trust root.
pub struct Services {
    pub environment: EnvironmentLookup,
    /// Replaces the Anthropic Messages endpoint. The profile's authorized destination follows it.
    pub anthropic_endpoint: Option<String>,
    /// Replaces the OpenAI chat-completions endpoint, with the same rule.
    pub openai_endpoint: Option<String>,
    /// Extra trusted root certificates (DER), in addition to the bundled public roots.
    pub extra_trust_roots: Vec<Vec<u8>>,
    pub cancel: CancelToken,
}

impl Services {
    pub fn production(cancel: CancelToken) -> Services {
        Services {
            environment: Arc::new(|name| std::env::var_os(name)),
            anthropic_endpoint: None,
            openai_endpoint: None,
            extra_trust_roots: Vec::new(),
            cancel,
        }
    }
}

fn default_build_revision() -> String {
    format!("bench-{}", env!("CARGO_PKG_VERSION"))
}

/// A refusal before anything was sent.
struct Refusal(String);

impl From<String> for Refusal {
    fn from(message: String) -> Refusal {
        Refusal(message)
    }
}

/// Runs the command line `words` (without the program name) and returns the exit status.
/// Normal output goes to `out`, refusals and usage errors to `err`.
pub fn main_with(
    words: &[String],
    services: &Services,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let command = match args::parse(words) {
        Ok(command) => command,
        Err(message) => {
            let _ = writeln!(err, "bench: {}", sanitize_to(&message, MESSAGE_LIMIT));
            let _ = writeln!(err, "try `bench help`");
            return EXIT_REFUSED;
        }
    };
    let result = match command {
        Command::Help => {
            let _ = write!(out, "{USAGE}");
            return EXIT_COMPLETED;
        }
        Command::Plan(options) => plan(&options, services, out),
        Command::Run(options) => run(&options, services, out),
    };
    match result {
        Ok(status) => status,
        Err(Refusal(message)) => {
            let _ = writeln!(err, "bench: {}", sanitize_to(&message, MESSAGE_LIMIT));
            let _ = writeln!(err, "nothing was sent");
            EXIT_REFUSED
        }
    }
}

struct Prepared {
    corpus: corpus::PlannedCorpus,
    arm_a: ArmProfile,
    arm_b: ArmProfile,
    experiment: Experiment,
    request_cap: Option<u32>,
}

fn prepare(options: &Options, services: &Services) -> Result<Prepared, Refusal> {
    if options.mode == Mode::Recorded {
        return Err(Refusal(
            "recorded mode is a reserved label: no recorded provider responses are wired to \
             this command"
                .to_string(),
        ));
    }
    let corpus = corpus::load(options)?;
    let overrides = EndpointOverrides {
        anthropic: services.anthropic_endpoint.as_deref(),
        openai: services.openai_endpoint.as_deref(),
    };
    let arm_a = build_profile(&options.profile_a, options.timeout_seconds, overrides)
        .map_err(|error| format!("profile a: {error}"))?;
    let arm_b = build_profile(&options.profile_b, options.timeout_seconds, overrides)
        .map_err(|error| format!("profile b: {error}"))?;
    if arm_a.profile.profile_version() == arm_b.profile.profile_version() {
        return Err(Refusal("the two profiles must differ".to_string()));
    }

    let build_revision = options
        .build_revision
        .clone()
        .unwrap_or_else(default_build_revision);
    let source_context = format!(
        "synthetic-corpus mode-{} settings-temperature-0-max-output-tokens-1024",
        options.mode.label()
    );
    let mut experiment = Experiment::new(
        corpus.version.clone(),
        source_context,
        corpus.instruction_version.clone(),
        arm_a.profile.profile_id(),
        arm_a.profile.profile_version(),
        arm_b.profile.profile_id(),
        arm_b.profile.profile_version(),
        build_revision,
    )?;
    if options.build_revision.is_none() {
        experiment.set_unknown_fields(vec!["source_revision".to_string()])?;
    }

    let maximum = (corpus.cases.len() as u64 * 2 * u64::from(options.attempts_per_side))
        .min(u64::from(MAX_REQUEST_CAP)) as u32;
    let request_cap = match (options.request_cap, options.mode) {
        (Some(cap), _) => Some(cap),
        (None, Mode::Fake) => Some(maximum),
        _ => None,
    };
    Ok(Prepared {
        corpus,
        arm_a,
        arm_b,
        experiment,
        request_cap,
    })
}

fn plan(options: &Options, services: &Services, out: &mut dyn Write) -> Result<i32, Refusal> {
    let prepared = prepare(options, services)?;
    let prior = match &options.journal {
        Some(path) => PriorJournal::read(path)?,
        None => None,
    };
    let maximum = (prepared.corpus.cases.len() as u64 * 2 * u64::from(options.attempts_per_side))
        .min(u64::from(MAX_REQUEST_CAP)) as u32;
    let view = PlanView {
        mode: options.mode,
        experiment: &prepared.experiment,
        corpus_size: prepared.corpus.corpus_size,
        arm_a: &prepared.arm_a,
        arm_b: &prepared.arm_b,
        cases: &prepared.corpus.cases,
        attempts_per_side: options.attempts_per_side,
        request_cap: prepared.request_cap.unwrap_or(maximum),
        reruns: &options.reruns,
        prior: prior.as_ref(),
    };
    let _ = write!(out, "{}", output::render_plan(&view));
    if options.mode == Mode::Live {
        let _ = writeln!(
            out,
            "\nlive run needs: --execute-network, --request-cap, --allow-destination and \
             --credential-env for each profile; none of it is checked by plan"
        );
    }
    Ok(EXIT_COMPLETED)
}

/// Refuses a rerun that names a case outside the plan or a side with no dispatched attempt, before
/// the journal is created or written, so an invalid invocation never pins a configuration.
fn check_reruns_before_open(
    options: &Options,
    prepared: &Prepared,
    journal: &Path,
) -> Result<(), Refusal> {
    if options.reruns.is_empty() {
        return Ok(());
    }
    let prior = PriorJournal::read(journal)?;
    for key in &options.reruns {
        if !prepared
            .corpus
            .cases
            .iter()
            .any(|case| case.case_id() == key.case_id)
        {
            return Err(Refusal(format!(
                "rerun target {} is not in the plan",
                key.case_id
            )));
        }
        let dispatched = prior
            .as_ref()
            .is_some_and(|prior| prior.has_dispatched(&key.case_id, key.arm));
        if !dispatched {
            return Err(Refusal(format!(
                "rerun target {} has no dispatched attempt to rerun",
                key.case_id
            )));
        }
    }
    Ok(())
}

/// What a live run needs before a single request is reserved.
struct LiveSetup {
    transport: HostTransport,
}

fn live_setup(
    options: &Options,
    services: &Services,
    arms: [&ArmProfile; 2],
) -> Result<LiveSetup, Refusal> {
    if !options.execute_network {
        return Err(Refusal(
            "live mode sends synthetic fixture text to the selected providers; pass \
             --execute-network to allow it"
                .to_string(),
        ));
    }
    if options.request_cap.is_none() {
        return Err(Refusal(
            "live mode requires an explicit --request-cap".to_string(),
        ));
    }
    let labels = ["a", "b"];
    for (arm, label) in arms.iter().zip(labels) {
        if !options.allow_destinations.contains(&arm.spec.protocol) {
            return Err(Refusal(format!(
                "the destination of profile {label} is not granted; choosing a profile grants \
                 nothing, pass --allow-destination {}",
                args::protocol_label(arm.spec.protocol)
            )));
        }
    }
    for granted in &options.allow_destinations {
        if !arms.iter().any(|arm| arm.spec.protocol == *granted) {
            return Err(Refusal(
                "--allow-destination names a protocol no selected profile uses".to_string(),
            ));
        }
    }
    let mut resolver = EnvCredentialResolver::with_lookup({
        let environment = services.environment.clone();
        move |name| environment(name)
    });
    for (reference, variable) in &options.credential_env {
        if !arms.iter().any(|arm| arm.spec.credential_ref == *reference) {
            return Err(Refusal(
                "--credential-env names a reference no selected profile uses".to_string(),
            ));
        }
        resolver = resolver.map_reference(reference.clone(), variable.clone());
    }
    for (arm, label) in arms.iter().zip(labels) {
        if resolver.resolve(&arm.spec.credential_ref).is_err() {
            return Err(Refusal(format!(
                "no usable credential for profile {label}: map its credential reference with \
                 --credential-env <ref>=<VAR> and set that variable to a non-empty secret"
            )));
        }
    }
    let mut policy = DestinationPolicy::new();
    for arm in arms {
        policy = policy
            .approve_credential(&arm.endpoint, &arm.spec.credential_ref)
            .map_err(|_| Refusal("a destination could not be approved".to_string()))?;
    }
    let mut builder = HostTransport::builder(policy, Arc::new(resolver));
    for root in &services.extra_trust_roots {
        builder = builder.trust_root_der(root.clone());
    }
    let transport = builder
        .build()
        .map_err(|_| Refusal("the secure transport could not be set up".to_string()))?;
    Ok(LiveSetup { transport })
}

fn run(options: &Options, services: &Services, out: &mut dyn Write) -> Result<i32, Refusal> {
    let prepared = prepare(options, services)?;
    let live = match options.mode {
        Mode::Live => Some(live_setup(
            options,
            services,
            [&prepared.arm_a, &prepared.arm_b],
        )?),
        _ => None,
    };
    let request_cap = prepared
        .request_cap
        .ok_or_else(|| Refusal("an explicit --request-cap is required".to_string()))?;
    let limits = ExecutorLimits::new(options.attempts_per_side, request_cap)
        .map_err(|error| Refusal(error.to_string()))?;
    let journal = options
        .journal
        .as_ref()
        .ok_or_else(|| Refusal("--journal is required for `run`".to_string()))?;

    // Live: only the destinations granted on the command line. Fake mode has no transport and
    // reaches no destination, so its arms' own origins are all the rules are asked about.
    let granted: Vec<String> = [&prepared.arm_a, &prepared.arm_b]
        .iter()
        .filter(|arm| {
            options.mode != Mode::Live || options.allow_destinations.contains(&arm.spec.protocol)
        })
        .map(|arm| arm.origin.clone())
        .collect();
    let authority = ScratchAuthority::new(
        &granted,
        prepared.arm_a.profile.clone(),
        prepared.arm_b.profile.clone(),
    )?;

    let adapter_for = |arm: &ArmProfile, label: &str| -> Box<dyn ProviderAdapter> {
        match &live {
            Some(live) => live_adapter(arm, &live.transport),
            None => Box::new(SyntheticAdapter::new(arm.spec.protocol, label)),
        }
    };
    let adapter_a = adapter_for(&prepared.arm_a, "a");
    let adapter_b = adapter_for(&prepared.arm_b, "b");

    check_reruns_before_open(options, &prepared, journal)?;
    let mut executor = Executor::open(journal, prepared.experiment.clone())
        .map_err(|error| Refusal(error.to_string()))?;
    let clock = SystemClock::new();
    let report = executor
        .run(RunRequest {
            cases: &prepared.corpus.cases,
            arm_a: ArmBinding {
                profile: &prepared.arm_a.profile,
                adapter: adapter_a.as_ref(),
                authorizer: &authority,
            },
            arm_b: ArmBinding {
                profile: &prepared.arm_b.profile,
                adapter: adapter_b.as_ref(),
                authorizer: &authority,
            },
            controls: RunControls {
                clock: &clock,
                cancel: &services.cancel,
                dispatch_limits: DispatchLimits::default(),
            },
            limits,
            reruns: &options.reruns,
        })
        .map_err(|error| Refusal(error.to_string()))?;

    let view = ReportView {
        mode: options.mode,
        experiment: executor.experiment(),
        arm_a: &prepared.arm_a,
        arm_b: &prepared.arm_b,
        report: &report,
    };
    let _ = write!(out, "{}", output::render_report(&view));
    Ok(if output::all_completed(&report) {
        EXIT_COMPLETED
    } else {
        EXIT_INCOMPLETE
    })
}
