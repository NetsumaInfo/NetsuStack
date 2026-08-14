import XCTest
@testable import PortlyCompanion

final class PortlyServiceClientTests: XCTestCase {
    func testDecodesServiceStatus() throws {
        let payload = ##"{"ok":true,"data":{"projects":[{"id":"prj_1","name":"Portly","color":"#338CFF","servers":[{"id":"srv_1","name":"website","port":3000,"state":"running","healthy":true,"pid":42}]}]}}"##
        let envelope = try JSONDecoder().decode(PortlyStatusEnvelope.self, from: Data(payload.utf8))

        XCTAssertTrue(envelope.ok)
        XCTAssertEqual(envelope.data.projects.first?.name, "Portly")
        XCTAssertEqual(envelope.data.projects.first?.servers.first?.port, 3000)
        XCTAssertTrue(envelope.data.projects.first?.servers.first?.isRunning == true)
    }

    func testDemoModeSupportsStartAndStopWithoutAService() {
        let initial = CompanionDemo.projects
        let api = initial[0].servers[1]
        XCTAssertFalse(api.isRunning)

        let started = CompanionDemo.applying("/start", to: api.id, in: initial)
        XCTAssertTrue(started[0].servers[1].isRunning)
        XCTAssertTrue(started[0].servers[1].healthy)
        XCTAssertNotNil(started[0].servers[1].pid)

        let stopped = CompanionDemo.applying("/stop", to: api.id, in: started)
        XCTAssertFalse(stopped[0].servers[1].isRunning)
        XCTAssertFalse(stopped[0].servers[1].healthy)
        XCTAssertNil(stopped[0].servers[1].pid)
    }

    func testDemoModeRestartKeepsServerRunning() {
        let website = CompanionDemo.projects[0].servers[0]
        let restarted = CompanionDemo.applying("/restart", to: website.id, in: CompanionDemo.projects)

        XCTAssertTrue(restarted[0].servers[0].isRunning)
        XCTAssertTrue(restarted[0].servers[0].healthy)
    }
}
