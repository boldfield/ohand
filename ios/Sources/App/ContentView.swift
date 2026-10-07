import SwiftUI

struct ContentView: View {
    var body: some View {
        VStack(spacing: 16) {
            Text("Oh And M1 Probe")
                .font(.largeTitle)
                .fontWeight(.bold)
                .padding()

            Text("Milestone 1 probe application")
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
                    Text("com.boldfield.ohand.app")
                        .font(.caption)
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
