import SwiftUI

struct ContentView: View {
    let bundleIdentifier = Bundle.main.bundleIdentifier ?? "unknown"

    var buildTargetName: String {
        #if OHAND_BUILD_TARGET_CONTROL
        "OhAndControl"
        #else
        "OhAndApp"
        #endif
    }

    var buildTargetDescription: String {
        #if OHAND_BUILD_TARGET_CONTROL
        "Control-entry build variant reserved for system-control handoff"
        #else
        "Production application with capture and management UI"
        #endif
    }

    var body: some View {
        VStack(spacing: 16) {
            Text("Oh And M1")
                .font(.largeTitle)
                .fontWeight(.bold)
                .padding()

            Text(buildTargetDescription)
                .font(.body)
                .foregroundColor(.secondary)

            Spacer()

            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("Platform:")
                    Spacer()
                    Text("iOS \(UIDevice.current.systemVersion)")
                }
                HStack {
                    Text("Build Target:")
                    Spacer()
                    Text(buildTargetName)
                }
                HStack {
                    Text("Bundle ID:")
                    Spacer()
                    Text(bundleIdentifier)
                        .font(.caption)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .padding()
            .background(Color(.systemGray6))
            .cornerRadius(8)

            Spacer()
        }
        .padding()
    }
}

#Preview {
    ContentView()
}
