import Foundation

struct PortlyStatusEnvelope: Decodable {
    let ok: Bool
    let data: PortlyServiceStatus
}

struct PortlyServiceStatus: Decodable {
    let projects: [CompanionProject]
}

struct CompanionProject: Decodable, Identifiable {
    let id: String
    let name: String
    let color: String
    let servers: [CompanionServer]
}

struct CompanionServer: Decodable, Identifiable {
    let id: String
    let name: String
    let port: Int?
    let state: String
    let healthy: Bool
    let pid: Int?
    let cpuPercent: Double?
    let memoryBytes: UInt64?
    let residentMemoryBytes: UInt64?
    let processCount: Int?

    var isRunning: Bool {
        state == "running" || state == "starting" || state == "unhealthy"
    }
}

enum CompanionDemo {
    static let projects = [
        CompanionProject(
            id: "demo_portly",
            name: "Portly Demo",
            color: "#338CFF",
            servers: [
                CompanionServer(
                    id: "demo_web",
                    name: "website",
                    port: 3000,
                    state: "running",
                    healthy: true,
                    pid: 42117,
                    cpuPercent: 3,
                    memoryBytes: 148_000_000,
                    residentMemoryBytes: 112_000_000,
                    processCount: 2
                ),
                CompanionServer(
                    id: "demo_api",
                    name: "api",
                    port: 8787,
                    state: "stopped",
                    healthy: false,
                    pid: nil,
                    cpuPercent: nil,
                    memoryBytes: nil,
                    residentMemoryBytes: nil,
                    processCount: nil
                )
            ]
        )
    ]

    static func applying(
        _ action: String,
        to serverID: String,
        in projects: [CompanionProject]
    ) -> [CompanionProject] {
        projects.map { project in
            CompanionProject(
                id: project.id,
                name: project.name,
                color: project.color,
                servers: project.servers.map { server in
                    guard server.id == serverID else { return server }

                    let isStopping = action == "/stop"
                    return CompanionServer(
                        id: server.id,
                        name: server.name,
                        port: server.port,
                        state: isStopping ? "stopped" : "running",
                        healthy: !isStopping,
                        pid: isStopping ? nil : (server.pid ?? 42118),
                        cpuPercent: isStopping ? nil : (server.cpuPercent ?? 2),
                        memoryBytes: isStopping ? nil : (server.memoryBytes ?? 126_000_000),
                        residentMemoryBytes: isStopping ? nil : (server.residentMemoryBytes ?? 96_000_000),
                        processCount: isStopping ? nil : (server.processCount ?? 2)
                    )
                }
            )
        }
    }
}

private struct PortlyActionTarget: Encodable {
    let server: String
}

enum PortlyServiceError: LocalizedError {
    case invalidResponse
    case serviceRejected(String)

    var errorDescription: String? {
        switch self {
        case .invalidResponse:
            return "Portly returned an invalid response."
        case .serviceRejected(let message):
            return message
        }
    }
}

enum PortlyServiceClient {
    static let serviceURL = URL(string: "http://127.0.0.1:7737")!

    static func status() async throws -> PortlyServiceStatus {
        let (data, response) = try await URLSession.shared.data(from: serviceURL.appendingPathComponent("status"))
        guard let response = response as? HTTPURLResponse, response.statusCode == 200 else {
            throw PortlyServiceError.invalidResponse
        }
        let envelope = try JSONDecoder().decode(PortlyStatusEnvelope.self, from: data)
        guard envelope.ok else { throw PortlyServiceError.serviceRejected("Portly rejected the status request.") }
        return envelope.data
    }

    static func perform(_ action: String, serverID: String) async throws {
        var request = URLRequest(url: serviceURL.appendingPathComponent(action))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(PortlyActionTarget(server: serverID))

        let (_, response) = try await URLSession.shared.data(for: request)
        guard let response = response as? HTTPURLResponse, response.statusCode == 200 else {
            throw PortlyServiceError.serviceRejected("Portly could not \(action.dropFirst()) this server.")
        }
    }
}
