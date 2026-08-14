import AppKit
import Foundation

/// Dock vs menu-bar visibility. Portly must keep at least one of them.
struct AppPresentation: Equatable {
    var showInDock: Bool
    var showMenuBar: Bool

    static let defaultShowInDock = true
    static let defaultShowMenuBar = true

    var usesAccessoryPolicy: Bool { !showInDock }

    var activationPolicy: NSApplication.ActivationPolicy {
        usesAccessoryPolicy ? .accessory : .regular
    }

    mutating func setShowInDock(_ value: Bool) {
        showInDock = value
        if !showInDock, !showMenuBar {
            showMenuBar = true
        }
    }

    mutating func setShowMenuBar(_ value: Bool) {
        showMenuBar = value
        if !showMenuBar, !showInDock {
            showInDock = true
        }
    }

    static func load(from defaults: UserDefaults = .standard) -> AppPresentation {
        var presentation = AppPresentation(
            showInDock: bool(defaults, PortlyPreferences.showInDockKey, default: defaultShowInDock),
            showMenuBar: bool(defaults, PortlyPreferences.showMenuBarItemKey, default: defaultShowMenuBar)
        )
        if !presentation.showInDock, !presentation.showMenuBar {
            presentation.showMenuBar = true
        }
        return presentation
    }

    func persist(to defaults: UserDefaults = .standard) {
        defaults.set(showInDock, forKey: PortlyPreferences.showInDockKey)
        defaults.set(showMenuBar, forKey: PortlyPreferences.showMenuBarItemKey)
    }

    func apply(
        to app: NSApplication = .shared,
        defaults: UserDefaults = .standard,
        activateIfRegular: Bool = false
    ) {
        persist(to: defaults)
        let wasActive = app.isActive
        let shouldActivate = wasActive || (activateIfRegular && !usesAccessoryPolicy)
        _ = app.setActivationPolicy(activationPolicy)
        if shouldActivate {
            DispatchQueue.main.async {
                app.activate(ignoringOtherApps: true)
            }
        }
    }

    static func applyFromUserDefaults(
        defaults: UserDefaults = .standard,
        app: NSApplication = .shared
    ) -> AppPresentation {
        let presentation = load(from: defaults)
        presentation.apply(to: app, defaults: defaults)
        return presentation
    }

    private static func bool(_ defaults: UserDefaults, _ key: String, default fallback: Bool) -> Bool {
        defaults.object(forKey: key) as? Bool ?? fallback
    }
}
