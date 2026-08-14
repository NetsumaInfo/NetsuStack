@testable import PortlyApp
import XCTest

final class AppPresentationTests: XCTestCase {
    func testDefaultsKeepBothEntryPoints() {
        let presentation = AppPresentation(showInDock: true, showMenuBar: true)

        XCTAssertFalse(presentation.usesAccessoryPolicy)
        XCTAssertEqual(presentation.activationPolicy, .regular)
    }

    func testHidingDockUsesAccessoryPolicyAndKeepsMenuBar() {
        var presentation = AppPresentation(showInDock: true, showMenuBar: true)
        presentation.setShowInDock(false)

        XCTAssertFalse(presentation.showInDock)
        XCTAssertTrue(presentation.showMenuBar)
        XCTAssertTrue(presentation.usesAccessoryPolicy)
        XCTAssertEqual(presentation.activationPolicy, .accessory)
    }

    func testHidingDockWhileMenuBarHiddenRestoresMenuBar() {
        var presentation = AppPresentation(showInDock: true, showMenuBar: false)
        presentation.setShowInDock(false)

        XCTAssertFalse(presentation.showInDock)
        XCTAssertTrue(presentation.showMenuBar)
        XCTAssertTrue(presentation.usesAccessoryPolicy)
    }

    func testHidingMenuBarWhileDockHiddenRestoresDock() {
        var presentation = AppPresentation(showInDock: false, showMenuBar: true)
        presentation.setShowMenuBar(false)

        XCTAssertTrue(presentation.showInDock)
        XCTAssertFalse(presentation.showMenuBar)
        XCTAssertFalse(presentation.usesAccessoryPolicy)
        XCTAssertEqual(presentation.activationPolicy, .regular)
    }

    func testHidingMenuBarWhileDockVisibleLeavesDockAlone() {
        var presentation = AppPresentation(showInDock: true, showMenuBar: true)
        presentation.setShowMenuBar(false)

        XCTAssertTrue(presentation.showInDock)
        XCTAssertFalse(presentation.showMenuBar)
        XCTAssertFalse(presentation.usesAccessoryPolicy)
    }

    func testMissingUserDefaultsTreatBothAsEnabled() {
        let defaults = isolatedDefaults()

        let presentation = AppPresentation.load(from: defaults)

        XCTAssertTrue(presentation.showInDock)
        XCTAssertTrue(presentation.showMenuBar)
    }

    func testLoadReadsExplicitFalseDock() {
        let defaults = isolatedDefaults()
        defaults.set(false, forKey: PortlyPreferences.showInDockKey)
        defaults.set(true, forKey: PortlyPreferences.showMenuBarItemKey)

        let presentation = AppPresentation.load(from: defaults)

        XCTAssertFalse(presentation.showInDock)
        XCTAssertTrue(presentation.showMenuBar)
        XCTAssertTrue(presentation.usesAccessoryPolicy)
    }

    func testLoadRepairsBothEntryPointsHidden() {
        let defaults = isolatedDefaults()
        defaults.set(false, forKey: PortlyPreferences.showInDockKey)
        defaults.set(false, forKey: PortlyPreferences.showMenuBarItemKey)

        let presentation = AppPresentation.load(from: defaults)

        XCTAssertFalse(presentation.showInDock)
        XCTAssertTrue(presentation.showMenuBar)
        XCTAssertTrue(presentation.usesAccessoryPolicy)
    }

    func testPersistRoundTrip() {
        let defaults = isolatedDefaults()
        let original = AppPresentation(showInDock: false, showMenuBar: true)

        original.persist(to: defaults)

        XCTAssertEqual(AppPresentation.load(from: defaults), original)
    }

    private func isolatedDefaults() -> UserDefaults {
        let name = "portly.tests.presentation.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock {
            defaults.removePersistentDomain(forName: name)
        }
        return defaults
    }
}
