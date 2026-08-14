import SwiftUI

@main
struct PortlyCompanionApp: App {
    @StateObject private var model = CompanionModel()

    var body: some Scene {
        WindowGroup {
            CompanionView()
                .environmentObject(model)
                .frame(minWidth: 680, minHeight: 420)
        }
        .defaultSize(width: 820, height: 520)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Refresh") {
                    Task { await model.refresh() }
                }
                .keyboardShortcut("r", modifiers: .command)
            }
        }
    }
}

@MainActor
final class CompanionModel: ObservableObject {
    enum ConnectionState: Equatable {
        case connecting
        case connected
        case unavailable(String)
    }

    @Published private(set) var projects: [CompanionProject] = []
    @Published private(set) var connectionState: ConnectionState = .connecting
    @Published private(set) var activeAction: String?
    @Published private(set) var isDemoMode = false

    func refresh() async {
        guard !isDemoMode else { return }
        do {
            let status = try await PortlyServiceClient.status()
            projects = status.projects
            connectionState = .connected
        } catch {
            projects = []
            connectionState = .unavailable(error.localizedDescription)
        }
    }

    func perform(_ action: String, on server: CompanionServer) async {
        activeAction = server.id
        defer { activeAction = nil }

        if isDemoMode {
            try? await Task.sleep(for: .milliseconds(250))
            projects = CompanionDemo.applying(action, to: server.id, in: projects)
            return
        }

        do {
            try await PortlyServiceClient.perform(action, serverID: server.id)
            try? await Task.sleep(for: .milliseconds(350))
            await refresh()
        } catch {
            connectionState = .unavailable(error.localizedDescription)
        }
    }

    func enterDemoMode() {
        isDemoMode = true
        projects = CompanionDemo.projects
        connectionState = .connected
    }

    func exitDemoMode() async {
        isDemoMode = false
        connectionState = .connecting
        await refresh()
    }
}

private struct CompanionView: View {
    @EnvironmentObject private var model: CompanionModel
    private let columns = [GridItem(.adaptive(minimum: 300), spacing: 16)]

    var body: some View {
        NavigationStack {
            Group {
                switch model.connectionState {
                case .connecting:
                    ProgressView("Connecting to Portly…")
                case .unavailable(let message):
                    unavailableView(message)
                case .connected:
                    dashboard
                }
            }
            .background(Color(nsColor: .windowBackgroundColor))
            .toolbar {
                ToolbarItem {
                    Button {
                        Task { await model.refresh() }
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }
            }
        }
        .task {
            await model.refresh()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                await model.refresh()
            }
        }
    }

    private var dashboard: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                dashboardHeader

                if model.isDemoMode {
                    demoBanner
                }

                if model.projects.isEmpty {
                    emptyProjects
                } else {
                    ForEach(model.projects) { project in
                        VStack(alignment: .leading, spacing: 12) {
                            Text(project.name)
                                .font(.headline)
                                .foregroundStyle(.secondary)

                            LazyVGrid(columns: columns, alignment: .leading, spacing: 16) {
                                ForEach(project.servers) { server in
                                    CompanionServerCard(
                                        server: server,
                                        isBusy: model.activeAction == server.id,
                                        perform: { action in
                                            Task { await model.perform(action, on: server) }
                                        }
                                    )
                                }
                            }
                        }
                    }
                }
            }
            .padding(24)
            .frame(maxWidth: 1_100, alignment: .leading)
        }
    }

    private var dashboardHeader: some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Portly")
                    .font(.system(size: 30, weight: .bold, design: .rounded))
                Text("Your local development services, at a glance.")
                    .foregroundStyle(.secondary)
            }

            Spacer()

            Label("Connected", systemImage: "checkmark.circle.fill")
                .font(.callout.weight(.medium))
                .foregroundStyle(.green)
                .padding(.horizontal, 12)
                .padding(.vertical, 7)
                .background(.green.opacity(0.12), in: Capsule())

            if !model.isDemoMode {
                Button("Explore Demo") {
                    model.enterDemoMode()
                }
                .buttonStyle(.bordered)
            }
        }
    }

    private var demoBanner: some View {
        HStack(spacing: 12) {
            Image(systemName: "play.rectangle.fill")
                .font(.title2)
                .foregroundStyle(.blue)

            VStack(alignment: .leading, spacing: 2) {
                Text("Demo Mode")
                    .font(.headline)
                Text("Try every control safely. These sample services never affect your Mac.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Spacer()

            Button("Exit Demo") {
                Task { await model.exitDemoMode() }
            }
            .buttonStyle(.bordered)
        }
        .padding(14)
        .background(.blue.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(.blue.opacity(0.18))
        }
    }

    private var emptyProjects: some View {
        VStack(spacing: 12) {
            Image(systemName: "shippingbox")
                .font(.system(size: 30))
                .foregroundStyle(.secondary)
            Text("No Portly projects")
                .font(.headline)
            Text("Add a project in Portly, or explore the demo to try every control.")
                .font(.callout)
                .foregroundStyle(.secondary)
            Button("Explore Demo") {
                model.enterDemoMode()
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity)
        .padding(40)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: 16))
    }

    private func unavailableView(_ message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "bolt.slash.fill")
                .font(.system(size: 36))
                .foregroundStyle(.orange)
            Text("Portly service is not running")
                .font(.title2.bold())
            Text("Connect to the local Portly service, or explore every feature in Demo Mode.\n\(message)")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .frame(maxWidth: 460)

            Button("Explore Demo") {
                model.enterDemoMode()
            }
            .buttonStyle(.borderedProminent)

            VStack(spacing: 10) {
                HStack {
                    Button("Try Again") {
                        Task { await model.refresh() }
                    }
                    Link("Get Portly", destination: URL(string: "https://portly.melvynx.dev")!)
                }
            }
        }
        .padding(40)
    }
}

private struct CompanionServerCard: View {
    let server: CompanionServer
    let isBusy: Bool
    let perform: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 10) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 10, height: 10)
                    .shadow(color: statusColor.opacity(0.5), radius: 3)
                    .accessibilityHidden(true)

                Text(server.name)
                    .font(.title3.bold())

                Spacer()

                Text(server.state.capitalized)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(statusColor)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 5)
                    .background(statusColor.opacity(0.12), in: Capsule())
            }

            HStack(spacing: 18) {
                if let port = server.port {
                    ServerMetric(label: "PORT", value: "\(port)")
                }
                if let pid = server.pid {
                    ServerMetric(label: "PID", value: "\(pid)")
                }
                if let cpu = server.cpuPercent {
                    ServerMetric(
                        label: "CPU",
                        value: "\(cpu.formatted(.number.precision(.fractionLength(0))))%"
                    )
                }
                if let resident = server.residentMemoryBytes {
                    ServerMetric(
                        label: "MEMORY",
                        value: ByteCountFormatter.string(fromByteCount: Int64(resident), countStyle: .memory)
                    )
                }
            }

            Spacer()

            HStack {
                Text(server.port.map { "localhost:\($0)" } ?? "No port assigned")
                    .font(.caption.monospaced())
                    .foregroundStyle(.tertiary)

                Spacer()

                if isBusy {
                    ProgressView()
                        .controlSize(.small)
                } else if server.isRunning {
                    Button("Restart") { perform("/restart") }
                    Button("Stop") { perform("/stop") }
                } else {
                    Button("Start") { perform("/start") }
                        .buttonStyle(.borderedProminent)
                }
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, minHeight: 170, alignment: .topLeading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
        .overlay {
            RoundedRectangle(cornerRadius: 16)
                .stroke(Color.primary.opacity(0.08))
        }
    }

    private var statusColor: Color {
        if server.healthy { return .green }
        if server.isRunning { return .orange }
        return .secondary
    }
}

private struct ServerMetric: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
            Text(value)
                .font(.callout.monospacedDigit().weight(.medium))
                .lineLimit(1)
        }
    }
}
