import SwiftUI

struct ContentView: View {
    let bundleIdentifier = Bundle.main.bundleIdentifier ?? "unknown"

    var body: some View {
        VStack(spacing: 16) {
            Text("Oh And M1")
                .font(.largeTitle)
                .fontWeight(.bold)
                .padding()

            Text("Production application with capture and management UI")
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
                    Text("OhAndApp")
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
