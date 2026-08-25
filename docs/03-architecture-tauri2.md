# Architecture cible Tauri 2

## 1. Vue d’ensemble

```text
React/WebView2 ── Tauri IPC ──┐
                              │
CLI netsustack ── HTTP ───────┼── AppService ── Supervisor ── ServerRuntime
                              │                       │
Config watcher ───────────────┘                       ├─ ConPTY + Job Object
                                                     ├─ health/logs/metrics
                                                     └─ ports/Docker/notifications
```

L’UI ne parle pas à l’API HTTP. Les commandes Tauri et les routes HTTP sont deux adaptateurs minces vers le même `AppService`. Il est interdit d’implémenter une règle métier dans React, dans un handler Tauri ou dans un handler Axum.

## 2. Arborescence cible

```text
NetsuStack/
├─ package.json
├─ vite.config.ts
├─ tsconfig.json
├─ src/
│  ├─ app/App.tsx
│  ├─ app/routes.ts
│  ├─ components/
│  ├─ features/
│  │  ├─ projects/
│  │  ├─ servers/
│  │  ├─ terminal/
│  │  ├─ temporary/
│  │  ├─ resources/
│  │  ├─ ports/
│  │  ├─ settings/
│  │  └─ agent-setup/
│  ├─ lib/ipc.ts
│  ├─ lib/contracts.ts
│  ├─ stores/supervisorStore.ts
│  └─ styles/
├─ src-tauri/
│  ├─ src/main.rs
│  ├─ src/lib.rs
│  ├─ src/commands.rs
│  ├─ src/tray.rs
│  ├─ src/lifecycle.rs
│  ├─ capabilities/main.json
│  ├─ tauri.conf.json
│  └─ Cargo.toml
├─ crates/
│  ├─ netsustack-domain/src/{config,models,ids,timeouts,memory}.rs
│  ├─ netsustack-config/src/{paths,store,watch,migrate}.rs
│  ├─ netsustack-supervisor/src/{service,supervisor,runtime,health,logs,diagnostics}.rs
│  ├─ netsustack-windows/src/{conpty,job,processes,ports,metrics,shell,autostart}.rs
│  ├─ netsustack-api/src/{server,routes,auth,envelope}.rs
│  └─ netsustack-cli/src/{main,commands,client,render,launch}.rs
├─ fixtures/
│  ├─ echo-server/
│  ├─ child-tree/
│  ├─ ansi-terminal/
│  ├─ crash-loop/
│  └─ memory-growth/
└─ tests/
   ├─ contracts/
   ├─ windows-integration/
   └─ e2e/
```

## 3. Responsabilités des crates

### `netsustack-domain`

Pure, sans Tauri ni Win32. Contient les structures sérialisables, valeurs par défaut, validations, résolution ID/nom, transitions d’état autorisées, parsing mémoire/timeout et versions de schéma.

### `netsustack-config`

Chemins, sérialisation JSON pretty/sorted, écriture atomique, permissions du jeton API, migrations et surveillance. Un changement externe valide publie un nouveau snapshot ; un fichier invalide n’écrase jamais l’état en mémoire et produit une erreur visible.

### `netsustack-supervisor`

Possède les runtimes et expose des méthodes métier. Utilise des traits `ProcessBackend`, `PortBackend`, `MetricsBackend`, `Notifier` afin de tester sans Win32. Le runtime est asynchrone Tokio ; aucun mutex n’est tenu pendant un `await`.

### `netsustack-windows`

Seul emplacement autorisé pour `unsafe` et les appels `windows` crate. Chaque wrapper Win32 est petit, documente les handles possédés et implémente `Drop`. Les erreurs gardent code Win32 et contexte.

### `netsustack-api`

Axum bindé explicitement sur `SocketAddrV4(127.0.0.1, api_port)`. Il ne crée aucun runtime et ne contient aucune règle métier. Toutes les réponses utilisent une enveloppe stable.

### `netsustack-cli`

Binaire autonome léger. `status` tente `/ping`, lance l’exécutable installé si absent, attend jusqu’à 20 s, puis exécute la commande. Les sorties JSON n’incluent aucun texte parasite sur stdout ; diagnostics sur stderr.

## 4. État concurrent

`AppState` contient des `Arc` vers `AppService`, `Supervisor`, `ConfigStore` et `ApiServerHandle`. Chaque `ServerRuntime` possède sa machine d’état dans une tâche dédiée et reçoit des messages sur un canal MPSC : `Start`, `Stop`, `Restart`, `Input`, `Resize`, `UpdateConfig`, `Shutdown`.

Cette approche évite les transitions concurrentes `start/stop/restart`. Le superviseur n’édite jamais directement l’intérieur d’un runtime : il envoie une commande et reçoit un résultat typé.

## 5. IPC Tauri

Les opérations demande/réponse utilisent `invoke`. Les snapshots et changements peu fréquents utilisent les events Tauri. Le flux terminal utilise `tauri::ipc::Channel`.

Commandes minimales :

```text
get_snapshot
add_project / update_project / remove_project
add_server / update_server / remove_server
start_target / stop_target / restart_target
run_temporary / run_action
set_memory_limit
query_port / list_ports / kill_port / take_over_port
attach_terminal / detach_terminal / terminal_input / terminal_resize / clear_terminal
open_path / open_url / reveal_config
agent_setup_status / install_agent_skill / install_agent_rules
check_for_updates / install_update / quit_app
```

Chaque commande : arguments possédés, retour `Result<T, AppError>`, inscription dans `generate_handler![]`. `main.rs` reste un passthrough vers `lib.rs::run()`.

## 6. Flux de données

1. Au lancement, config + migrations sont chargées.
2. Le superviseur crée un runtime arrêté pour chaque serveur persistant.
3. Le serveur API et le tray démarrent.
4. React appelle `get_snapshot`, puis s’abonne à `netsustack://snapshot`.
5. Toute mutation passe par `AppService`, persiste si nécessaire, puis publie un snapshot révisionné.
6. Les métriques ne provoquent au maximum qu’une publication toutes les 2 s.
7. Un terminal s’attache avec la révision/transcript courant, puis reçoit des chunks ordonnés.

Chaque snapshot porte `revision: u64`. React ignore toute révision plus ancienne pour éviter un retour d’état après une réponse lente.

## 7. Terminal dans React

Utiliser `@xterm/xterm`, `@xterm/addon-fit`, `@xterm/addon-search` et `@xterm/addon-web-links`.

- `terminalRegistry.ts` garde une instance xterm par serveur tant que le WebView vit.
- Rust garde un transcript ANSI borné séparé du log texte.
- À l’attachement : Rust envoie `ReplayStarted`, le transcript, `ReplayFinished`, puis les chunks live.
- Chaque chunk possède une séquence ; le renderer ignore les doublons et demande un replay s’il détecte un trou.
- Le redimensionnement est debouncé à 50 ms et appelle `ResizePseudoConsole`.
- Le terminal reste utilisable même si la santé ou les métriques sont en erreur.

## 8. Dépendances prévues

### Rust

- `tauri` v2 et plugins officiels : single-instance, autostart, updater, notification, opener, dialog, window-state.
- `tokio`, `axum`, `reqwest`, `serde`, `serde_json`, `thiserror`, `tracing`.
- `clap` pour le CLI, `notify` pour la config, `uuid`, `chrono`.
- `windows` pour ConPTY, Job Objects, IP Helper, Toolhelp/PSAPI et tokens.

### Frontend

- React + TypeScript + Vite.
- `@tauri-apps/api` v2 et bindings des plugins réellement appelés depuis React.
- xterm packages.
- Fluent System Icons.
- Vitest, Testing Library et axe-core.

Les dépendances de gestion d’état ou de composants ne sont ajoutées que si les primitives React deviennent insuffisantes. Le store initial peut utiliser Zustand, mais aucune logique métier ne doit y être déplacée.

## 9. Capabilities Tauri

La fenêtre `main` reçoit uniquement : fenêtre de base, events, dialog de dossier, opener URL/path, notifications, updater et window-state. L’UI n’obtient pas `shell:allow-execute` ni un accès filesystem général : l’exécution et les fichiers passent par les commandes Rust contrôlées.

Cette séparation évite qu’une compromission WebView devienne une exécution de commande arbitraire. Les chemins de projet choisis par l’utilisateur sont validés côté Rust.

## 10. Règles d’architecture

- Une seule définition Rust des modèles wire/persistés ; types TypeScript générés ou vérifiés par tests de fixtures JSON.
- Aucun sidecar superviseur en v1.
- Aucun polling React inférieur à 2 s ; événements après le snapshot initial.
- Aucun appel Win32 dans les handlers Tauri/HTTP.
- Aucun `unwrap()` sur une entrée, un handle ou une opération runtime.
- Toute opération destructive revalide la cible juste avant l’action.
- Quit, update et uninstall utilisent le même chemin de shutdown ordonné.

Références : [Tauri system tray](https://v2.tauri.app/learn/system-tray/), [single instance](https://v2.tauri.app/plugin/single-instance/), [autostart](https://v2.tauri.app/plugin/autostart/), [updater](https://v2.tauri.app/plugin/updater/).
