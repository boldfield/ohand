import UIKit

/// Builds the system share sheet for a prepared export and reports how it ended. The sheet is the only place that can
/// tell whether an activity completed, so the caller must pass the ending to `ExportService.finishShare` and report
/// success only from that outcome.
public enum ExportShareSheet {
    public static func ending(completed: Bool, error: Error?) -> ExportShareEnding {
        if error != nil { return .failed }
        return completed ? .completed : .cancelled
    }

    /// `onEnding` runs once, on the main thread, when the sheet is dismissed or an activity finishes.
    public static func makeController(
        for export: PreparedExport, onEnding: @escaping (ExportShareEnding) -> Void
    ) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: export.shareItems, applicationActivities: nil)
        controller.completionWithItemsHandler = { _, completed, _, error in
            onEnding(ending(completed: completed, error: error))
        }
        return controller
    }
}
