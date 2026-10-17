import Foundation
import SQLite3
import UIKit
import XCTest
@testable import OhAndCoreBridge
@testable import OhAndServices

// MARK: - Test doubles

/// Holds a step until the test opens the gate.
private final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?
    private var isOpen = false

    func wait() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            if isOpen {
                lock.unlock()
                continuation.resume()
            } else {
                self.continuation = continuation
                lock.unlock()
            }
        }
    }

    func open() {
        lock.lock()
        isOpen = true
        let waiting = continuation
        continuation = nil
        lock.unlock()
        waiting?.resume()
    }
}

/// Records which steps ran, how many ran at once, and how often each ran.
private final class StepLog: @unchecked Sendable {
    private let lock = NSLock()
    private var names: [String] = []
    private var running = 0
    private var maximumRunning = 0

    var order: [String] {
        lock.lock()
        defer { lock.unlock() }
        return names
    }

    var maximumConcurrent: Int {
        lock.lock()
        defer { lock.unlock() }
        return maximumRunning
    }

    func count(of name: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return names.filter { $0 == name }.count
    }

    func begin(_ name: String) {
        lock.lock()
        names.append(name)
        running += 1
        maximumRunning = max(maximumRunning, running)
        lock.unlock()
    }

    func end() {
        lock.lock()
        running -= 1
        lock.unlock()
    }
}

/// The durable effects of an app's steps, which survive the "process" that applied them.
private final class DurableEffects: @unchecked Sendable {
    private let lock = NSLock()
    private var applied: Set<String> = []
    private var applications: [String: Int] = [:]

    var appliedSteps: Set<String> {
        lock.lock()
        defer { lock.unlock() }
        return applied
    }

    func applications(of name: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return applications[name] ?? 0
    }

    func apply(_ name: String) {
        lock.lock()
        applied.insert(name)
        applications[name, default: 0] += 1
        lock.unlock()
    }
}

private func lifecycleWaitUntil(timeout: TimeInterval = 10, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}

// MARK: - Ordering, serialization and interruption

final class LifecycleCoordinatorTests: XCTestCase {
    private func makeCoordinator(
        timeout: TimeInterval = 10,
        center: NotificationCenter = NotificationCenter(),
        log: StepLog,
        outcomes: [LifecycleStep: LifecycleStepOutcome] = [:],
        steps: [LifecycleStep] = LifecycleStep.allCases
    ) -> LifecycleCoordinator {
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: timeout, notificationCenter: center)
        for step in steps {
            let outcome = outcomes[step] ?? .completed
            coordinator.setAction(
                { _ in
                    log.begin(step.rawValue)
                    defer { log.end() }
                    return outcome
                }, for: step)
        }
        return coordinator
    }

    func testLaunchRunsEveryStepInRecoveryOrder() async {
        let log = StepLog()
        let coordinator = makeCoordinator(log: log)
        let report = await coordinator.reconcile(.launch)
        XCTAssertEqual(
            log.order, ["ingress", "reset", "jobs", "deletion", "reminders", "retention", "promptEligibility", "health"])
        XCTAssertEqual(report.results.map(\.step), LifecycleStep.allCases)
        XCTAssertTrue(report.results.allSatisfy { $0.outcome == .completed })
        XCTAssertEqual(report.triggers, [.launch])
    }

    func testAStepWithNoActionIsReportedNotConfigured() async {
        let log = StepLog()
        let coordinator = makeCoordinator(log: log, steps: [.ingress])
        let report = await coordinator.reconcile(.foreground)
        XCTAssertEqual(report.outcome(of: .ingress), .completed)
        XCTAssertEqual(report.outcome(of: .promptEligibility), .notConfigured)
        XCTAssertEqual(report.outcome(of: .health), .notConfigured)
        XCTAssertEqual(log.order, ["ingress"])
    }

    func testAFailedOrDeferredStepDoesNotStopTheStepsAfterIt() async {
        let log = StepLog()
        let coordinator = makeCoordinator(
            log: log, outcomes: [.ingress: .failed("synthetic"), .deletion: .deferred("synthetic")])
        let report = await coordinator.reconcile(.launch)
        XCTAssertEqual(report.outcome(of: .ingress), .failed("synthetic"))
        XCTAssertEqual(report.outcome(of: .deletion), .deferred("synthetic"))
        XCTAssertEqual(log.order.count, LifecycleStep.allCases.count)
        XCTAssertEqual(report.outcome(of: .health), .completed)
    }

    func testAHungStepTimesOutAndLaterStepsStillRun() async {
        let log = StepLog()
        let coordinator = makeCoordinator(timeout: 0.2, log: log, steps: LifecycleStep.allCases.filter { $0 != .jobs })
        coordinator.setAction(
            { _ in
                log.begin("jobs")
                defer { log.end() }
                try? await Task.sleep(nanoseconds: 60_000_000_000)
                return .completed
            }, for: .jobs)
        let started = Date()
        let report = await coordinator.reconcile(.launch)
        XCTAssertLessThan(Date().timeIntervalSince(started), 10, "the pass does not wait for the hung step")
        XCTAssertEqual(report.outcome(of: .jobs), .timedOut)
        XCTAssertEqual(report.outcome(of: .deletion), .completed)
        XCTAssertEqual(report.outcome(of: .health), .completed)
    }

    func testRequestReturnsBeforeSlowStepsFinish() async {
        let gate = Gate()
        let log = StepLog()
        let coordinator = makeCoordinator(log: log, steps: [])
        coordinator.setAction(
            { _ in
                log.begin("ingress")
                defer { log.end() }
                await gate.wait()
                return .completed
            }, for: .ingress)
        coordinator.request(.launch)
        let began = await lifecycleWaitUntil { log.count(of: "ingress") == 1 }
        XCTAssertTrue(began)
        XCTAssertEqual(coordinator.finishedPassCount, 0, "the caller got control back while the step was still running")
        gate.open()
        let finished = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(finished)
    }

    func testOverlappingTriggersSerializeAndShareOneFollowingPass() async {
        let gate = Gate()
        let log = StepLog()
        let coordinator = makeCoordinator(log: log, steps: LifecycleStep.allCases.filter { $0 != .ingress })
        coordinator.setAction(
            { _ in
                log.begin("ingress")
                defer { log.end() }
                if log.count(of: "ingress") == 1 {
                    await gate.wait()
                }
                return .completed
            }, for: .ingress)

        let first = Task { await coordinator.reconcile(.launch) }
        let began = await lifecycleWaitUntil { log.count(of: "ingress") == 1 }
        XCTAssertTrue(began)

        let overlapping = (0..<4).map { _ in Task { await coordinator.reconcile(.foreground) } }
        let queued = await lifecycleWaitUntil { coordinator.queuedRequestCount == 4 }
        XCTAssertTrue(queued, "every overlapping call waits for the next pass")
        XCTAssertEqual(log.count(of: "ingress"), 1, "nothing starts while a pass runs")

        gate.open()
        let firstReport = await first.value
        var laterReports: [LifecyclePassReport] = []
        for task in overlapping {
            laterReports.append(await task.value)
        }
        XCTAssertEqual(firstReport.triggers, [.launch])
        XCTAssertEqual(Set(laterReports.map(\.triggers)), [[.foreground]])
        XCTAssertEqual(laterReports.count, 4)
        XCTAssertTrue(laterReports.allSatisfy { $0 == laterReports[0] }, "the overlapping calls share one pass")
        XCTAssertEqual(log.count(of: "ingress"), 2, "exactly one rerun, however many triggers arrived")
        XCTAssertEqual(log.maximumConcurrent, 1, "steps never overlap")
        XCTAssertEqual(coordinator.finishedPassCount, 2)
    }

    func testRepeatedActivationRunsAFullPassEachTimeAndConverges() async {
        let effects = DurableEffects()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 10, notificationCenter: NotificationCenter())
        for step in LifecycleStep.allCases {
            coordinator.setAction(
                { _ in
                    effects.apply(step.rawValue)
                    return .completed
                }, for: step)
        }
        for _ in 0..<3 {
            let report = await coordinator.reconcile(.foreground)
            XCTAssertTrue(report.results.allSatisfy { $0.outcome == .completed })
        }
        XCTAssertEqual(effects.appliedSteps, Set(LifecycleStep.allCases.map(\.rawValue)))
        XCTAssertEqual(coordinator.finishedPassCount, 3)
    }

    func testInterruptionAtEachRecoveryBoundaryConvergesOnTheNextRun() async {
        for crashingStep in LifecycleStep.allCases {
            for crashAfterEffect in [false, true] {
                let effects = DurableEffects()
                let doomed = LifecycleCoordinator(stepTimeoutSeconds: 10, notificationCenter: NotificationCenter())
                for step in LifecycleStep.allCases {
                    doomed.setAction(
                        { [weak doomed] _ in
                            if step == crashingStep {
                                if crashAfterEffect { effects.apply(step.rawValue) }
                                doomed?.shutDown()
                                return .completed
                            }
                            effects.apply(step.rawValue)
                            return .completed
                        }, for: step)
                }
                let interrupted = await doomed.reconcile(.launch)
                let crashIndex = LifecycleStep.allCases.firstIndex(of: crashingStep)!
                for (index, step) in LifecycleStep.allCases.enumerated() {
                    if index < crashIndex {
                        XCTAssertEqual(interrupted.outcome(of: step), .completed, "\(step) finished before the crash")
                    } else {
                        XCTAssertEqual(
                            interrupted.outcome(of: step), .interrupted,
                            "\(step) did not finish when the pass died at \(crashingStep)")
                    }
                }
                let beforeRestart = effects.appliedSteps
                XCTAssertEqual(
                    beforeRestart.contains(crashingStep.rawValue), crashAfterEffect,
                    "only an effect applied before the crash is durable")
                XCTAssertTrue(
                    LifecycleStep.allCases[(crashIndex + 1)...].allSatisfy { !beforeRestart.contains($0.rawValue) },
                    "nothing after the crash ran")

                let restarted = LifecycleCoordinator(stepTimeoutSeconds: 10, notificationCenter: NotificationCenter())
                for step in LifecycleStep.allCases {
                    restarted.setAction(
                        { _ in
                            effects.apply(step.rawValue)
                            return .completed
                        }, for: step)
                }
                let recovered = await restarted.reconcile(.launch)
                XCTAssertTrue(
                    recovered.results.allSatisfy { $0.outcome == .completed },
                    "the next launch finishes every step after a crash at \(crashingStep)")
                XCTAssertEqual(effects.appliedSteps, Set(LifecycleStep.allCases.map(\.rawValue)))
                XCTAssertEqual(effects.applications(of: crashingStep.rawValue), crashAfterEffect ? 2 : 1)
            }
        }
    }

    func testShutDownReleasesAHungStepAndEveryQueuedCaller() async {
        let gate = Gate()
        let log = StepLog()
        let coordinator = makeCoordinator(log: log, steps: LifecycleStep.allCases.filter { $0 != .ingress })
        coordinator.setAction(
            { _ in
                log.begin("ingress")
                defer { log.end() }
                await gate.wait()
                return .completed
            }, for: .ingress)
        let running = Task { await coordinator.reconcile(.launch) }
        let began = await lifecycleWaitUntil { log.count(of: "ingress") == 1 }
        XCTAssertTrue(began)
        let waiting = Task { await coordinator.reconcile(.foreground) }
        let queued = await lifecycleWaitUntil { coordinator.queuedRequestCount == 1 }
        XCTAssertTrue(queued)

        coordinator.shutDown()
        let runningReport = await running.value
        let waitingReport = await waiting.value
        XCTAssertEqual(runningReport.outcome(of: .ingress), .interrupted)
        XCTAssertTrue(waitingReport.results.allSatisfy { $0.outcome == .interrupted })
        XCTAssertEqual(log.count(of: "jobs"), 0, "nothing runs after shutdown")

        let after = await coordinator.reconcile(.launch)
        XCTAssertTrue(after.results.allSatisfy { $0.outcome == .interrupted })
        gate.open()
    }

    // MARK: system time changes

    func testTimeChangeRunsOnlyTheReminderAndPromptSteps() async {
        let log = StepLog()
        let coordinator = makeCoordinator(log: log)
        let report = await coordinator.reconcile(.systemTimeChange)
        XCTAssertEqual(log.order, ["reminders", "promptEligibility"])
        XCTAssertEqual(report.results.map(\.step), [.reminders, .promptEligibility])
    }

    func testRepeatedTimeChangeCallbacksShareOnePass() async {
        let gate = Gate()
        let log = StepLog()
        let coordinator = makeCoordinator(log: log, steps: [.promptEligibility])
        coordinator.setAction(
            { _ in
                log.begin("reminders")
                defer { log.end() }
                if log.count(of: "reminders") == 1 {
                    await gate.wait()
                }
                return .completed
            }, for: .reminders)

        let trigger = coordinator.timeChangeTrigger
        trigger()
        let began = await lifecycleWaitUntil { log.count(of: "reminders") == 1 }
        XCTAssertTrue(began)
        for _ in 0..<5 {
            trigger()
        }
        let queued = await lifecycleWaitUntil { coordinator.queuedRequestCount == 5 }
        XCTAssertTrue(queued)
        gate.open()
        let finished = await lifecycleWaitUntil { coordinator.finishedPassCount == 2 }
        XCTAssertTrue(finished)
        let settled = await lifecycleWaitUntil { coordinator.queuedRequestCount == 0 }
        XCTAssertTrue(settled)
        XCTAssertEqual(log.count(of: "reminders"), 2, "five callbacks during a pass cost one further pass")
        XCTAssertEqual(log.count(of: "ingress"), 0)
        XCTAssertEqual(log.count(of: "jobs"), 0)
    }

    // MARK: lifecycle hooks

    func testStartingActiveRequestsALaunchPassAndActivationRequestsForegroundPasses() async {
        let center = NotificationCenter()
        let log = StepLog()
        let coordinator = makeCoordinator(center: center, log: log, steps: [.ingress])
        coordinator.start(isActive: true)
        coordinator.start(isActive: true)
        let launched = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(launched)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.launch], "starting twice launches once")

        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await lifecycleWaitUntil { coordinator.finishedPassCount == 2 }
        XCTAssertTrue(foregrounded)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.foreground])

        coordinator.shutDown()
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(coordinator.finishedPassCount, 2, "a stopped coordinator ignores activation")
    }

    func testStartingInactiveWaitsForActivation() async {
        let center = NotificationCenter()
        let log = StepLog()
        let coordinator = makeCoordinator(center: center, log: log, steps: [.ingress])
        coordinator.start(isActive: false)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(coordinator.finishedPassCount, 0)
        center.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(foregrounded)
        coordinator.shutDown()
    }

    // MARK: outcome mapping

    func testStepOutcomesNameWhetherWorkConverged() {
        XCTAssertEqual(LifecycleCoordinator.outcome(ofIngress: nil), .deferred("ingress_unavailable"))
        XCTAssertEqual(LifecycleCoordinator.outcome(ofIngress: IngressRecoveryReport()), .completed)
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofDeletion: DeletionReconcileReport(listingFailure: .coreUnavailable, items: [])),
            .deferred("deletion_listing_failed"))
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofDeletion: DeletionReconcileReport(listingFailure: nil, items: [])),
            .completed)
        XCTAssertEqual(
            LifecycleCoordinator.outcome(
                ofRetention: AudioRetentionSweepReport(listingFailure: .coreUnavailable, pressure: .normal, items: [])),
            .deferred("retention_listing_failed"))
        XCTAssertEqual(
            LifecycleCoordinator.outcome(
                ofRetention: AudioRetentionSweepReport(listingFailure: nil, pressure: .normal, items: [])),
            .completed)
    }

    func testPromptEligibilityOutcomeNamesWhyItHasNotConverged() {
        func report(
            permission: NotificationPermissionState = .usable, granted: Int = 0, deferred: Int = 0,
            withheld: Int = 0, failure: ReminderReconciliationFailure? = nil
        ) -> NotificationPermissionReport {
            NotificationPermissionReport(
                permission: permission, transition: .unchanged, trigger: .launch,
                readiness: NotificationReadiness.evaluate(permission: permission, reminders: []),
                promptsGranted: granted, promptsDeferred: deferred, promptsWithheld: withheld,
                reconciliationFailure: failure, converged: failure == nil)
        }
        XCTAssertEqual(LifecycleCoordinator.outcome(ofPrompts: nil), .deferred("prompts_awaiting_reminder_pass"))
        XCTAssertEqual(LifecycleCoordinator.outcome(ofPrompts: report()), .completed)
        XCTAssertEqual(LifecycleCoordinator.outcome(ofPrompts: report(granted: 2)), .completed)
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofPrompts: report(deferred: 1)), .deferred("prompts_awaiting_capacity"))
        XCTAssertEqual(LifecycleCoordinator.outcome(ofPrompts: report(withheld: 1)), .deferred("prompts_withheld"))
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofPrompts: report(permission: .denied)), .deferred("prompts_not_permitted"))
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofPrompts: report(failure: .coreUnavailable)),
            .deferred("prompts_pass_incomplete"))
    }

    func testHealthOutcomeSeparatesASnapshotFromAFailedRead() {
        XCTAssertEqual(
            LifecycleCoordinator.outcome(ofHealth: .failed(CoreFailure.handleClosed)), .deferred("health_read_failed"))
    }
}

// MARK: - Real Rust core

/// Runs the coordinator over a production-shaped root: the real core, the real services, and a fake only at the
/// operating system's edges (notification center, provider transport, authentication).
final class LifecycleCoordinatorRealCoreTests: XCTestCase {
    private static let routeID = "route-lifecycle"
    private static let timeZone = TimeZone(identifier: "UTC")!

    private var storageRoot: URL!
    private var baselineHandles = 0
    private var baselineBuffers = 0

    override func setUpWithError() throws {
        storageRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("lifecycle-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: storageRoot, withIntermediateDirectories: true)
        baselineHandles = CoreHandle.liveHandleCount
        baselineBuffers = CoreHandle.liveResultBufferCount
    }

    override func tearDownWithError() throws {
        XCTAssertEqual(CoreHandle.liveHandleCount, baselineHandles, "every core handle is closed")
        XCTAssertEqual(CoreHandle.liveResultBufferCount, baselineBuffers, "no result buffer leaks")
        try? FileManager.default.removeItem(at: storageRoot)
    }

    func testLaunchReconcilesEveryServiceAndRecoversACaptureAnEarlierProcessLeftStaged() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let captureID = UUID().uuidString.lowercased()
        let capturedAt = Date()
        let staging = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        _ = try staging.stage(
            IngressRecord(
                captureID: captureID, text: "Pick up the synthetic parcel", audio: nil,
                context: captureContext(at: capturedAt)))
        XCTAssertEqual(try staging.listRecords().recordCaptureIDs, [captureID], "the earlier process died after staging")

        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        root.setAppActiveForTests(true)

        let launch = await coordinator.reconcile(.launch)
        XCTAssertEqual(launch.outcome(of: .ingress), .completed)
        XCTAssertEqual(launch.outcome(of: .jobs), .requested)
        XCTAssertEqual(launch.outcome(of: .deletion), .completed)
        XCTAssertEqual(launch.outcome(of: .reminders), .completed)
        XCTAssertEqual(launch.outcome(of: .retention), .completed)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(launch.outcome(of: .health), .completed)
        if case .snapshot? = coordinator.latestHealth {
        } else {
            XCTFail("the health step left no snapshot to read, got \(String(describing: coordinator.latestHealth))")
        }
        XCTAssertEqual(try staging.listRecords().recordCaptureIDs, [], "the staged capture was imported and cleared")
        XCTAssertNotNil(root.lastReminderReport, "the reminder step ran the root's reminder pass")

        var repeated: CompositionSaveOutcome?
        root.saveText(captureID: captureID, text: "Pick up the synthetic parcel", capturedAt: capturedAt) {
            repeated = $0
        }
        XCTAssertTrue(pump { repeated != nil })
        if case .saved(_, _, let alreadySaved)? = repeated {
            XCTAssertTrue(alreadySaved, "recovery imported the capture exactly once")
        } else {
            XCTFail("expected the capture to be saved already, got \(String(describing: repeated))")
        }

        let again = await coordinator.reconcile(.foreground)
        XCTAssertEqual(again.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(again.outcome(of: .health), .completed)
        XCTAssertEqual(again.outcome(of: .ingress), .completed)
        XCTAssertEqual(again.outcome(of: .deletion), .completed)
        XCTAssertEqual(again.outcome(of: .retention), .completed)
        XCTAssertEqual(again.outcome(of: .reminders), .completed)
        XCTAssertTrue(pump { root.jobRunner.finishedDrainCount >= 1 }, "the job step started a drain")
    }

    func testForegroundActivationReachesTheCoordinatorThroughTheRootsNotificationCenter() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let lifecycle = NotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], lifecycleNotifications: lifecycle)
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        coordinator.start(isActive: false)

        lifecycle.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let reconciled = await lifecycleWaitUntil { coordinator.finishedPassCount >= 1 }
        XCTAssertTrue(reconciled)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.foreground])
        XCTAssertEqual(coordinator.lastReport?.outcome(of: .ingress), .completed)
    }

    func testConnectingACoordinatorThatIsNotAModuleOfTheRootIsRefused() throws {
        let coordinator = LifecycleCoordinator()
        let root = try makeRoot(modules: [])
        defer { root.shutDown() }
        XCTAssertThrowsError(try coordinator.connect(to: root)) { error in
            XCTAssertEqual(error as? LifecycleCoordinatorError, .notAttached)
        }
    }


    func testTheCoordinatorKeepsTheClientsItTookReachableForOtherCode() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        XCTAssertNil(coordinator.processingHealth)
        let root = try makeRoot(modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        try coordinator.connect(to: root)

        XCTAssertNotNil(coordinator.processingHealth, "a status surface reads health through the coordinator")
        XCTAssertNotNil(coordinator.deletionCleanup)
        XCTAssertNotNil(coordinator.audioRetention)
        let observed = ObservedHealth()
        coordinator.setHealthObserver { observed.record($0) }

        let report = await coordinator.reconcile(.foreground)
        XCTAssertEqual(report.outcome(of: .health), .completed)
        let notified = await lifecycleWaitUntil { observed.count >= 1 }
        XCTAssertTrue(notified, "the observer hears the read a pass made")
        XCTAssertEqual(observed.last, coordinator.latestHealth)

        coordinator.processingHealth?.refresh()
        let again = await lifecycleWaitUntil { observed.count >= 2 }
        XCTAssertTrue(again, "a read the status surface asks for reaches the same observer")
        let cleaned = await coordinator.deletionCleanup?.cleanUp(itemID: "item-that-was-never-deleted")
        XCTAssertNotNil(cleaned, "the owned deletion service answers an immediate cleanup")
    }

    func testStartingTheRootDrivesLaunchAndForegroundOnlyThroughTheCoordinator() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let captureID = UUID().uuidString.lowercased()
        let staging = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        _ = try staging.stage(
            IngressRecord(
                captureID: captureID, text: "Water the synthetic fern", audio: nil,
                context: captureContext(at: Date())))

        let lifecycle = NotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], lifecycleNotifications: lifecycle, reachable: false)
        defer { root.shutDown() }
        // Stand in for the three steps the root's own observers would otherwise also run, so a second driver shows.
        try coordinator.connect(to: root)
        let ran = StepCounter()
        for step in [LifecycleStep.ingress, .jobs, .reminders] {
            coordinator.setAction({ _ in ran.increment(step.rawValue); return .completed }, for: step)
        }

        root.start(isActive: true)
        let launched = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(launched)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.launch])

        lifecycle.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await lifecycleWaitUntil { coordinator.finishedPassCount == 2 }
        XCTAssertTrue(foregrounded)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.foreground])
        try? await Task.sleep(nanoseconds: 500_000_000)

        XCTAssertEqual(coordinator.finishedPassCount, 2, "one pass per activation")
        for step in [LifecycleStep.ingress, .jobs, .reminders] {
            XCTAssertEqual(ran.count(of: step.rawValue), 2, "\(step) ran once per pass")
        }
        XCTAssertEqual(try staging.listRecords().recordCaptureIDs, [captureID], "the root did not recover ingress itself")
        XCTAssertEqual(root.jobRunner.startedDrainCount, 0, "the root did not activate the job runner itself")
        XCTAssertEqual(
            root.lastReminderReport?.trigger, .foreground,
            "the root ran no reminder pass itself; the only recorded pass is the prompt step's, for the last foreground")
    }

    func testStartingTheRootRunsTheRealOrderedPassOnLaunchAndForeground() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let captureID = UUID().uuidString.lowercased()
        let staging = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        _ = try staging.stage(
            IngressRecord(
                captureID: captureID, text: "Renew the synthetic library card", audio: nil,
                context: captureContext(at: Date())))

        let lifecycle = NotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], lifecycleNotifications: lifecycle)
        defer { root.shutDown() }
        root.start(isActive: true)

        let launched = await lifecycleWaitUntil { coordinator.finishedPassCount >= 1 }
        XCTAssertTrue(launched)
        let launch = try XCTUnwrap(coordinator.lastReport)
        XCTAssertEqual(launch.triggers, [.launch])
        XCTAssertEqual(launch.results.map(\.step), LifecycleStep.allCases)
        XCTAssertEqual(launch.outcome(of: .ingress), .completed)
        XCTAssertEqual(launch.outcome(of: .reminders), .completed)
        XCTAssertEqual(launch.outcome(of: .promptEligibility), .completed)
        XCTAssertEqual(launch.outcome(of: .health), .completed)
        XCTAssertEqual(try staging.listRecords().recordCaptureIDs, [], "the launch pass imported the staged capture")
        XCTAssertNotNil(root.lastReminderReport)

        lifecycle.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await lifecycleWaitUntil { coordinator.finishedPassCount >= 2 }
        XCTAssertTrue(foregrounded)
        XCTAssertEqual(coordinator.lastReport?.triggers, [.foreground])
        XCTAssertEqual(coordinator.lastReport?.outcome(of: .promptEligibility), .completed)
    }

    func testASlowLaunchPassNeverReactivatesTheJobRunnerAfterTheAppWentToTheBackground() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let lifecycle = NotificationCenter()
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], lifecycleNotifications: lifecycle, reachable: false)
        defer { root.shutDown() }
        try coordinator.connect(to: root)
        let ingressGate = Gate()
        coordinator.setAction({ _ in await ingressGate.wait(); return .completed }, for: .ingress)

        root.start(isActive: true)
        XCTAssertTrue(root.isAppActive)
        lifecycle.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        let backgrounded = await lifecycleWaitUntil { !root.isAppActive }
        XCTAssertTrue(backgrounded)
        ingressGate.open()

        let finished = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(finished)
        XCTAssertEqual(
            coordinator.lastReport?.outcome(of: .jobs), .deferred("app_not_active"),
            "the stale pass leaves a suspended runner suspended")
        try? await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(root.jobRunner.startedDrainCount, 0, "no drain started in the background")

        lifecycle.post(name: UIApplication.didBecomeActiveNotification, object: nil)
        let foregrounded = await lifecycleWaitUntil { coordinator.finishedPassCount == 2 }
        XCTAssertTrue(foregrounded)
        XCTAssertEqual(coordinator.lastReport?.outcome(of: .jobs), .requested, "the next activation starts the runner")
    }

    func testARootWithNoLifecycleModuleStillRunsItsOwnLaunchRecovery() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let captureID = UUID().uuidString.lowercased()
        let staging = IngressStagingStore(layout: layout, fileSystem: FileManagerIngressFileSystem())
        _ = try staging.stage(
            IngressRecord(
                captureID: captureID, text: "Book the synthetic dentist", audio: nil,
                context: captureContext(at: Date())))
        let root = try makeRoot(modules: [], reachable: false)
        defer { root.shutDown() }
        root.start(isActive: true)

        XCTAssertTrue(
            pump { (try? staging.listRecords().recordCaptureIDs) == [] && root.lastReminderReport != nil },
            "without a lifecycle module the root recovers and reconciles by itself")
        XCTAssertTrue(pump { root.jobRunner.startedDrainCount >= 1 }, "and activates the job runner")
    }

    func testPromptEligibilityWaitsWhileNotificationPermissionIsDenied() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let center = FakeNotificationCenter()
        center.authorizationStatus = .denied
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator], notificationCenter: center)
        defer { root.shutDown() }
        try coordinator.connect(to: root)

        let report = await coordinator.reconcile(.launch)
        XCTAssertEqual(report.outcome(of: .promptEligibility), .deferred("prompts_not_permitted"))
        XCTAssertEqual(report.outcome(of: .ingress), .completed, "a denied permission does not stop the other steps")
    }

    func testTimeChangeTriggerRunsTheRealReminderAndPromptStepsOnly() async throws {
        let layout = ProtectedStorageLayout(rootDirectory: storageRoot)
        try prepareStore(layout)
        let coordinator = LifecycleCoordinator(stepTimeoutSeconds: 30)
        let root = try makeRoot(modules: [coordinator])
        defer { root.shutDown() }
        try coordinator.connect(to: root)

        coordinator.timeChangeTrigger()
        let finished = await lifecycleWaitUntil { coordinator.finishedPassCount == 1 }
        XCTAssertTrue(finished)
        XCTAssertEqual(coordinator.lastReport?.results.map(\.step), [.reminders, .promptEligibility])
        XCTAssertEqual(coordinator.lastReport?.outcome(of: .reminders), .completed)
        XCTAssertEqual(coordinator.lastReport?.outcome(of: .promptEligibility), .completed)
    }

    // MARK: helpers

    private func makeConfiguration() -> CompositionConfiguration {
        CompositionConfiguration(
            storageRootPath: storageRoot.path, captureRouteID: Self.routeID, jobRetryDelaySeconds: 3600)
    }

    private func makeRoot(
        modules: [CompositionModule],
        lifecycleNotifications: NotificationCenter = NotificationCenter(),
        notificationCenter: FakeNotificationCenter = FakeNotificationCenter(),
        reachable: Bool = true
    ) throws -> ServiceCompositionRoot {
        let session = ReadAuthenticationSession(boundary: FakeReadAuthenticationBoundary())
        return try ServiceCompositionRoot(
            testing: makeConfiguration(),
            environment: CompositionEnvironment(
                timeZone: { Self.timeZone }, locale: { Locale(identifier: "en_US") }),
            adapters: .injectedForTests(
                sender: UnusedProviderSender(), readSession: session, notificationCenter: notificationCenter,
                reachability: FakeReachability(reachable: reachable), lifecycleNotifications: lifecycleNotifications),
            modules: modules)
    }

    private func captureContext(at capturedAt: Date) -> IngressCaptureContext {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        let stamp = formatter.string(from: capturedAt)
        return IngressCaptureContext(
            captureInstant: stamp, timezoneID: Self.timeZone.identifier, utcOffsetMinutes: 0, locale: "en_US",
            calendar: "gregorian", itemScope: "personal", routeID: Self.routeID, entryLocked: false, createdAt: stamp,
            sessionTopic: nil)
    }

    private func pump(timeout: TimeInterval = 15, until condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.01))
        }
        return condition()
    }

    /// Creates the stores and the schema with the real core, then adds the route a capture is saved under.
    private func prepareStore(_ layout: ProtectedStorageLayout) throws {
        _ = ProtectedStorageService(layout: layout).prepare()
        try CoreHandle(path: layout.databaseURL.path).close()
        let destination = #"["https://api.anthropic.com"]"#
        var database: OpaquePointer?
        XCTAssertEqual(sqlite3_open(layout.databaseURL.path, &database), SQLITE_OK)
        defer { sqlite3_close(database) }
        sqlite3_busy_timeout(database, 5000)
        let statements = [
            """
            INSERT INTO routes (route_id, route_name, scope, processing_destinations, created_at) VALUES \
            ('\(Self.routeID)', 'synthetic', 'personal', '\(destination)', '2026-10-08T09:00:00Z')
            """,
            """
            INSERT INTO route_authorizations (route_id, capability, authorized_destinations, created_at) VALUES \
            ('\(Self.routeID)', 'text_interpretation', '\(destination)', '2026-10-08T09:00:00Z')
            """,
        ]
        for statement in statements {
            var message: UnsafeMutablePointer<CChar>?
            let code = sqlite3_exec(database, statement, nil, nil, &message)
            let detail = message.map { String(cString: $0) } ?? ""
            sqlite3_free(message)
            XCTAssertEqual(code, SQLITE_OK, "seed failed: \(detail)")
            if code != SQLITE_OK { throw CompositionError.storageUnavailable }
        }
    }
}

private final class StepCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var counts: [String: Int] = [:]

    func increment(_ name: String) {
        lock.lock()
        counts[name, default: 0] += 1
        lock.unlock()
    }

    func count(of name: String) -> Int {
        lock.lock()
        defer { lock.unlock() }
        return counts[name] ?? 0
    }
}

private final class ObservedHealth: @unchecked Sendable {
    private let lock = NSLock()
    private var outcomes: [ProcessingHealthOutcome] = []

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return outcomes.count
    }

    var last: ProcessingHealthOutcome? {
        lock.lock()
        defer { lock.unlock() }
        return outcomes.last
    }

    func record(_ outcome: ProcessingHealthOutcome) {
        lock.lock()
        outcomes.append(outcome)
        lock.unlock()
    }
}

/// A provider transport for tests that never expect a provider request.
private final class UnusedProviderSender: ProviderRequestSender, @unchecked Sendable {
    func send(_ request: ProviderHTTPRequest) async throws -> ProviderHTTPResponse {
        XCTFail("the lifecycle pass sent a provider request")
        return ProviderHTTPResponse(status: 500, headers: [:], body: Data())
    }
}
