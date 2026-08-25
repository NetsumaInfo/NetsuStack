# Matrice d’équivalence Windows

## Légende

- **Identique** : même comportement observable.
- **Adapté** : même intention, primitive Windows différente.
- **Écart assumé** : impossibilité ou risque documenté.

| Portly | Source | NetsuStack Windows | Niveau |
| --- | --- | --- | --- |
| SwiftUI/AppKit | `PortlyApp.swift`, vues SwiftUI | Tauri 2 + React/TS + WebView2 | Adapté |
| Menu bar | `MenuBarExtra` | `TrayIconBuilder` + menu natif Tauri | Adapté |
| Dock visible/masqué | ActivationPolicy | Fenêtre visible/masquée + tray toujours disponible | Adapté |
| Instance unique | Convention app macOS | `tauri-plugin-single-instance` avant tout autre plugin | Identique |
| Vrai PTY | SwiftTerm `LocalProcess` | ConPTY + xterm.js | Adapté |
| Shell login `zsh -lc` | `ServerRuntime.spawn` | shell configurable ; PowerShell 7 si choisi, sinon `cmd.exe /D /S /C` | Adapté |
| Groupe POSIX | PID/PGID | Job Object avec `KILL_ON_JOB_CLOSE` | Adapté |
| SIGTERM puis SIGKILL | `stop()` | Ctrl+C via ConPTY, attente 5 s, `TerminateJobObject` | Adapté |
| Stop externe SIGTERM | `ExternalProcessController` | revalidation PID/creation time puis terminaison explicite ; pas d’escalade automatique | Écart assumé |
| `lsof` listeners/cwd | `PortInspector` | `GetExtendedTcpTable` v4/v6 + process query | Adapté |
| cwd externe | `lsof -d cwd` | inconnu ou inféré ; aucune lecture PEB fragile | Écart assumé |
| Docker Desktop | Docker CLI | même `docker ps/inspect/stop` avec `docker.exe` découvert sur PATH | Identique |
| `ps` + `proc_pid_rusage` | `ProcessMetrics` | Toolhelp/NT query + `GetProcessMemoryInfo` | Adapté |
| physical footprint | `ri_phys_footprint` | `PrivateUsage`, nommé « mémoire privée » | Écart assumé |
| RAM résidente | `ri_resident_size` | `WorkingSetSize` | Identique conceptuellement |
| CPU `%cpu` par arbre | `/bin/ps` | delta `GetProcessTimes`, 100 % par cœur | Identique conceptuellement |
| Config `~/.config/portly` | `PortlyPaths` | `%USERPROFILE%\.config\netsustack` | Adapté |
| FSEvents/DispatchSource | `ConfigStore` | crate `notify`, debounce 350 ms | Adapté |
| Network.framework | `HealthChecker` | Tokio TCP + Reqwest | Identique |
| API `127.0.0.1:7737` | `ControlServer` | Axum, même port/routes/enveloppes | Identique + renforcé |
| API sans token | loopback uniquement | token local obligatoire sauf `/ping` | Écart de sécurité volontaire |
| Swift ArgumentParser | `PortlyCLI` | `clap` derive | Identique |
| LaunchAgent `forever` | `ForeverManager` | plugin autostart / registre utilisateur | Adapté |
| Sparkle | `Updater.swift` | `tauri-plugin-updater` + signature Ed25519 | Adapté |
| Developer ID | `build.sh` | Authenticode OV/EV + timestamp | Adapté |
| `.app` + CLI embarqué | bundle | NSIS par utilisateur + CLI embarqué/copied | Adapté |
| Notifications macOS | `Notifications` | plugin notification Windows | Adapté |
| SF Symbols | `Project.icons` | table stable vers Fluent System Icons | Adapté |
| Finder/Activity Monitor | actions UI | Explorer/Task Manager | Adapté |
| StoreApp compagnon | `StoreApp/` | non prévu en v1 | Hors périmètre |
| Analytics minimale | `Analytics.swift` | désactivée par défaut ; aucune donnée sans consentement | Écart volontaire |

## Trois stratégies envisagées

### A. Cœur Rust partagé, app Tauri propriétaire du runtime — retenue

Le superviseur vit dans le processus Tauri. Les crates de domaine/runtime sont partagées avec le CLI, mais une seule instance possède les processus. Le CLI lance l’app si besoin puis utilise l’API loopback.

Avantages : parité exacte avec l’app macOS, une source de vérité, pas de sidecar à synchroniser, arrêt déterministe à Quit. Inconvénient : l’app doit rester vivante en tray.

### B. Daemon Rust séparé + UI Tauri cliente

Un daemon sans UI possède les processus, l’app et le CLI sont des clients.

Avantages : supervision indépendante du WebView, meilleur futur service Windows. Inconvénients : deux cycles de vie, installation/upgrade plus risqués, différence majeure avec Quit et `forever`, complexité inutile pour v1.

### C. Réutiliser le daemon Go Linux comme sidecar

La UI Tauri contrôlerait le code Go existant adapté à Windows.

Avantages : démarrage plus rapide sur API/CLI. Inconvénients : viole l’objectif Rust, deux toolchains, PTY/ports/métriques Windows à réécrire malgré tout, duplication durable.

## Décision

La stratégie A est normative. L’architecture doit conserver des limites de crates assez propres pour extraire ultérieurement le superviseur dans un daemon sans changer les modèles, l’API ou le renderer.
