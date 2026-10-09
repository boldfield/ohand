import Foundation
import OhAndCoreBridge

/// How a native capability run ended.
public enum JobCapabilityResult: Equatable, Sendable {
    /// The handler already settled the job through the core's own operations, under the lease it was given.
    case settled
    /// A fault expected to pass; the core counts it against the job's retry budget. `reason` is a short lowercase
    /// label (`[a-z0-9_]`), never captured content.
    case transientFailure(reason: String)
    /// A fault retrying cannot fix; the job ends `failed`.
    case permanentFailure(reason: String)
    /// The handler stopped at a checkpoint (for example because its task was cancelled); nothing is spent.
    case interrupted
}

/// The registration seam for a job type that runs on the device rather than in the core, such as transcription
/// (C05). A handler is given to `JobRunnerService` at creation; the service declares its `jobType` to the core, and
/// the drain hands every ready job of that type to `run`. Until a handler is supplied for a type, the core leaves
/// jobs of that type queued and reports them as `capability_unavailable`: no work is claimed, none is lost, and
/// nothing pretends the capability exists.
///
/// `run` executes in its own task. Honor task cancellation: a cancelled run is abandoned by the core and its job
/// put back at once.
public protocol JobCapabilityHandler: AnyObject, Sendable {
    var jobType: String { get }
    func run(_ job: JobCapabilityRun) async -> JobCapabilityResult
}
