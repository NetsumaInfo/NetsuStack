# NetsuStack Windows Implementation Plan

> **For agentic workers:** execute this plan task-by-task, keep the checkboxes as the source of progress, and run every stated verification before committing.

**Goal:** construire l’équivalent Windows complet de Portly avec Tauri 2, Rust et React/TypeScript.

**Architecture:** une application Tauri possède le superviseur et expose le même service métier à React via IPC et au CLI via HTTP loopback. Un workspace Rust sépare domaine, config, runtime, Win32, API et CLI afin que toutes les interfaces partagent les mêmes contrats.

**Tech Stack:** Tauri 2, Rust stable MSVC, Tokio, Axum, windows-rs, React, TypeScript, Vite, xterm.js, Vitest et tests d’intégration Windows.

---

## Ordre de livraison

Chaque tâche doit finir par un commit autonome. Ne pas commencer l’UI terminal avant que le harness ConPTY/Job Object soit vert. Ne pas publier d’installer avant les tests de shutdown et update handoff.

### Task 1: Initialiser le dépôt et les toolchains

**Files:**

- Create: `.gitignore`, `.editorconfig`, `rust-toolchain.toml`, `Cargo.toml`
- Create: `package.json`, `package-lock.json`, `tsconfig.json`, `vite.config.ts`
- Create: `src/index.tsx`, `src/app/App.tsx`
- Create: `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src-tauri/tauri.conf.json`
- Create: `src-tauri/capabilities/main.json`

- [x] Créer le fork GitHub `NetsumaInfo/NetsuStack`, conserver l’historique Portly et configurer `upstream`.
- [x] Créer une app Vite React TypeScript et Tauri v2 nommée NetsuStack, identifier `com.netsuma.netsustack`, fenêtre 1080×660, minimum 900×560.
- [x] Créer le workspace Cargo avec les six crates prévues, chacune compilant avec un `lib.rs` minimal.
- [x] Garder `src-tauri/src/main.rs` comme appel unique à `netsustack_app_lib::run()`.
- [x] Ajouter les scripts `check`, `test`, `build`, `tauri:dev` et `tauri:build`.
- [x] Exécuter `npm ci`, `npm run check`, `cargo test --workspace` et `npx tauri build --debug`.
- [x] Commit: `chore: initialize NetsuStack Tauri workspace`.

**Expected:** fenêtre vide démarrable, tous les packages compilent, aucune capability shell/fs globale.

### Task 2: Domaine, validations et fixtures JSON

**Files:**

- Create: `crates/netsustack-domain/src/config.rs`
- Create: `crates/netsustack-domain/src/models.rs`
- Create: `crates/netsustack-domain/src/ids.rs`
- Create: `crates/netsustack-domain/src/timeouts.rs`
- Create: `crates/netsustack-domain/src/memory.rs`
- Create: `crates/netsustack-domain/tests/contracts.rs`
- Create: `Tests/contracts/config-v1-minimal.json`, `Tests/contracts/status-complete.json` (racine `Tests` existante, compatible avec la casse Windows)

- [x] Écrire les tests de valeurs par défaut et anciennes configs.
- [x] Implémenter `Project`, `ServerConfig`, `ServerAction`, `NetsuStackConfig` et enums.
- [x] Écrire les tests de résolution ID/nom/qualifié et conflits insensibles à la casse.
- [x] Implémenter IDs préfixés et résolution.
- [x] Écrire les tables de tests mémoire/timeout puis les parseurs.
- [x] Ajouter tous les DTO runtime et tests de sérialisation camelCase/ISO-8601.
- [x] Exécuter `cargo test -p netsustack-domain`.
- [x] Commit: `feat: define shared NetsuStack contracts`.

**Expected:** les fixtures JSON se décodent et ré-encodent sans perte de champs normatifs.

### Task 3: Config store, chemins, migration et token

**Files:**

- Create: `crates/netsustack-config/src/paths.rs`
- Create: `crates/netsustack-config/src/store.rs`
- Create: `crates/netsustack-config/src/watch.rs`
- Create: `crates/netsustack-config/src/migrate.rs`
- Create: `crates/netsustack-config/src/token.rs`
- Create: `crates/netsustack-config/tests/store.rs`

- [x] Tester les chemins sous un profile utilisateur injecté.
- [x] Tester la création initiale et l’écriture atomique JSON pretty/sorted.
- [x] Tester backup+migration et conservation d’un fichier invalide.
- [x] Tester debounce 350 ms et exclusion des écritures internes.
- [x] Tester génération 256 bits, persistance et ACL du token sur Windows.
- [x] Implémenter store/watch/migrations/token jusqu’à réussite.
- [x] Exécuter `cargo test -p netsustack-config` sur Windows.
- [x] Commit: `feat: add atomic configuration store`.

**Expected:** aucune édition externe invalide ne remplace la dernière config valide.

### Task 4: Logs texte et transcript terminal

**Files:**

- Create: `crates/netsustack-supervisor/src/logs/plain.rs`
- Create: `crates/netsustack-supervisor/src/logs/ansi.rs`
- Create: `crates/netsustack-supervisor/src/logs/mod.rs`
- Create: `crates/netsustack-supervisor/tests/logs.rs`

- [x] Écrire les tests UTF-8 fragmenté, CSI, OSC, CR, CRLF, BEL et backspace.
- [x] Implémenter un décodeur incremental sans croissance non bornée.
- [x] Tester ring buffer, `tail`, changement de limite et rotation `.1.log`.
- [x] Implémenter les notes `[netsustack]` et timestamps fichier.
- [x] Tester transcript ANSI borné et replay séquencé.
- [x] Exécuter `cargo test -p netsustack-supervisor logs`.
- [x] Commit: `feat: capture terminal and plain logs`.

**Expected:** les logs CLI sont lisibles et le transcript conserve les couleurs pour xterm.

### Task 5: Backend Win32 ConPTY et Job Object

**Files:**

- Create: `crates/netsustack-windows/src/handles.rs`
- Create: `crates/netsustack-windows/src/conpty.rs`
- Create: `crates/netsustack-windows/src/job.rs`
- Create: `crates/netsustack-windows/src/shell.rs`
- Create: `fixtures/ansi-terminal/src/main.rs`
- Create: `fixtures/child-tree/src/main.rs`
- Create: `crates/netsustack-windows/tests/conpty_job.rs`

- [x] Tester input/output VT UTF-8 et terminal size initiale.
- [x] Encapsuler pipes, HPCON, process/thread et Job handles avec `Drop`.
- [x] Tester resize réel via `ResizePseudoConsole`.
- [x] Tester sélection auto pwsh/cmd et résolution `.cmd`.
- [x] Tester Ctrl+C coopératif puis fallback `TerminateJobObject` à 5 s.
- [x] Tester parent/enfant/petit-enfant et port libéré après fermeture du job.
- [x] Exécuter `cargo test -p netsustack-windows --test conpty_job -- --nocapture`.
- [x] Commit: `feat: supervise Windows process trees with ConPTY`.

**Expected:** zéro descendant et zéro handle restant après 100 cycles automatisés.

### Task 6: Machine d’état ServerRuntime

**Files:**

- Create: `crates/netsustack-supervisor/src/runtime.rs`
- Create: `crates/netsustack-supervisor/src/health.rs`
- Create: `crates/netsustack-supervisor/src/backoff.rs`
- Create: `fixtures/echo-server/src/main.rs`
- Create: `fixtures/crash-loop/src/main.rs`
- Create: `crates/netsustack-supervisor/tests/runtime.rs`

- [x] Écrire un fake `ProcessBackend` et les tests de toutes les transitions.
- [x] Tester backoff 1/2/4/8/16/30, budget et reset après 30 s sain.
- [x] Tester TCP IPv4, IPv6, HTTP 200–399, statut exact et timeout.
- [x] Tester trois échecs santé, `autoRestart=false`, manual start/restart.
- [x] Implémenter la boucle MPSC sérialisant toutes les commandes runtime.
- [x] Brancher le backend Windows et exécuter les tests réels fixture.
- [x] Commit: `feat: implement server runtime state machine`.

**Expected:** aucune combinaison start/stop/restart concurrente ne crée deux process trees.

### Task 7: Jobs temporaires et actions

**Files:**

- Modify: `crates/netsustack-supervisor/src/runtime.rs`
- Create: `crates/netsustack-supervisor/src/temporary.rs`
- Create: `crates/netsustack-supervisor/tests/temporary.rs`

- [x] Tester succès, code non nul, stop 130 et timeout 124.
- [x] Tester deadline monotone et timeout maximum 7 jours.
- [x] Tester conservation une heure avec horloge injectée.
- [x] Tester action héritant cwd/env/PORT/NETSUSTACK_SERVER.
- [x] Implémenter temporaire/action sans écriture config et sans auto-restart.
- [x] Commit: `feat: add supervised temporary jobs and actions`.

**Expected:** `wait` peut récupérer le résultat d’un job très court terminé avant son appel.

### Task 8: Ports, processus externes et Docker

**Files:**

- Create: `crates/netsustack-windows/src/ports.rs`
- Create: `crates/netsustack-windows/src/processes.rs`
- Create: `crates/netsustack-windows/src/docker.rs`
- Create: `crates/netsustack-supervisor/src/takeover.rs`
- Create: `fixtures/port-reuse/src/main.rs`
- Create: `crates/netsustack-windows/tests/ports.rs`
- Create: `crates/netsustack-supervisor/tests/takeover.rs`

- [ ] Tester `GetExtendedTcpTable` v4/v6 et déduplication.
- [ ] Tester identité PID + creation time + executable + port.
- [ ] Tester refus sur cible changée et processus protégé.
- [ ] Tester parsing Docker inspect pour HostPort exact et labels Compose.
- [ ] Tester takeover : terminate/stop container, 50×200 ms, start configuré.
- [ ] Commit: `feat: inspect and take over Windows ports safely`.

**Expected:** aucune action n’est envoyée à un PID qui ne correspond plus au snapshot.

### Task 9: Métriques, historique, garde et diagnostics

**Files:**

- Create: `crates/netsustack-windows/src/metrics.rs`
- Create: `crates/netsustack-supervisor/src/metrics.rs`
- Create: `crates/netsustack-supervisor/src/memory_guard.rs`
- Create: `crates/netsustack-supervisor/src/diagnostics.rs`
- Create: `fixtures/memory-growth/src/main.rs`
- Create: `crates/netsustack-supervisor/tests/metrics.rs`
- Create: `crates/netsustack-supervisor/tests/diagnostics.rs`

- [ ] Tester CPU delta, PrivateUsage, WorkingSetSize et agrégation du Job.
- [ ] Tester historique 150 points et enrichissement externe toutes les 10 s.
- [ ] Tester les seuils machine de la spécification et trois échantillons.
- [ ] Tester croissance médiane, pic isolé et doublons dev.
- [ ] Tester redémarrage groupé des seuls serveurs actifs du projet.
- [ ] Commit: `feat: add Windows resource diagnostics`.

**Expected:** une erreur d’accès à un PID externe ne supprime pas les métriques gérées.

### Task 10: Supervisor et AppService

**Files:**

- Create: `crates/netsustack-supervisor/src/supervisor.rs`
- Create: `crates/netsustack-supervisor/src/service.rs`
- Create: `crates/netsustack-supervisor/src/error.rs`
- Create: `crates/netsustack-supervisor/tests/service.rs`

- [ ] Tester sync config→runtime, suppression qui stoppe et hot reload.
- [ ] Tester CRUD, conflits noms/ports et résolution cibles.
- [ ] Tester snapshot révisionné et coalescing métriques.
- [ ] Tester shutdown ordonné et marqueur update.
- [ ] Implémenter tous les use-cases utilisés par IPC et HTTP.
- [ ] Commit: `feat: centralize supervisor application service`.

**Expected:** les adaptateurs n’ont besoin d’aucun accès direct aux runtimes.

### Task 11: API Axum et CLI

**Files:**

- Create: `crates/netsustack-api/src/server.rs`, `routes.rs`, `auth.rs`, `envelope.rs`
- Create: `crates/netsustack-api/tests/api.rs`
- Create: `crates/netsustack-cli/src/main.rs`, `commands.rs`, `client.rs`, `render.rs`, `launch.rs`
- Create: `crates/netsustack-cli/tests/cli.rs`

- [ ] Écrire un test pour chaque route et code HTTP.
- [ ] Tester 127.0.0.1 exclusivement, token, Origin, limite 1 MiB.
- [ ] Porter toutes les commandes/aliases/flags et rendus compact/detailed/JSON.
- [ ] Tester auto-launch, timeout 20 s, stdout JSON propre et exit codes wait.
- [ ] Tester `config --path-only` sans lancer l’app.
- [ ] Commit: `feat: expose loopback API and NetsuStack CLI`.

**Expected:** le CLI contrôle les mêmes objets que l’UI et ne lance jamais son propre superviseur.

### Task 12: Shell Tauri, lifecycle, tray et plugins

**Files:**

- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/commands.rs`
- Create: `src-tauri/src/tray.rs`, `src-tauri/src/lifecycle.rs`, `src-tauri/src/updater.rs`, `src-tauri/src/agent_setup.rs`
- Modify: `src-tauri/capabilities/main.json`, `src-tauri/tauri.conf.json`
- Create: `src-tauri/tests/lifecycle.rs`

- [ ] Enregistrer single-instance en premier, puis autostart/updater/notification/opener/window-state.
- [ ] Enregistrer chaque commande Tauri dans `generate_handler![]`.
- [ ] Tester fermeture→hide, tray→show/focus, Quit→shutdown complet.
- [ ] Implémenter menu tray dynamique et icône active.
- [ ] Implémenter setup agent par staging/rename/blocs balisés.
- [ ] Implémenter reprise update consommable une fois et expirante 10 min.
- [ ] Auditer capabilities/CSP et vérifier l’absence de shell/fs général.
- [ ] Commit: `feat: integrate NetsuStack with Tauri lifecycle`.

**Expected:** fermer la fenêtre conserve les serveurs ; Quit les arrête tous.

### Task 13: Store React, shell UI et navigation

**Files:**

- Create: `src/lib/contracts.ts`, `src/lib/ipc.ts`
- Create: `src/stores/supervisorStore.ts`
- Create: `src/app/AppShell.tsx`, `src/app/routes.ts`
- Create: `src/features/sidebar/*`, `src/features/projects/*`, `src/features/servers/*`
- Create: `src/styles/tokens.css`, `src/styles/app.css`
- Create: tests colocalisés `*.test.tsx`

- [ ] Générer/vérifier les types TS depuis les fixtures Rust.
- [ ] Tester snapshot initial, events, ordre des révisions et reconnexion.
- [ ] Construire sidebar/recherche/navigation/projet/serveur et états vides.
- [ ] Implémenter formulaires projet/serveur avec détection de commandes.
- [ ] Tester clavier, focus, labels et axe-core.
- [ ] Commit: `feat: build NetsuStack project interface`.

**Expected:** CRUD et contrôle de serveurs fonctionnent sans terminal ni dashboard final.

### Task 14: Terminal xterm et jobs temporaires UI

**Files:**

- Create: `src/features/terminal/TerminalSurface.tsx`
- Create: `src/features/terminal/terminalRegistry.ts`
- Create: `src/features/terminal/terminalClient.ts`
- Create: `src/features/temporary/TemporaryPage.tsx`, `TemporaryForm.tsx`
- Create: tests terminal/temporaire colocalisés

- [ ] Tester replay/live, séquences, doublons, trou et détachement.
- [ ] Brancher input, Ctrl+C, resize debounce, copy/search/web links.
- [ ] Appliquer palette/font/insets et reduced motion.
- [ ] Construire liste/détail/formulaire temporaire et actions Run Again/Remove.
- [ ] Commit: `feat: add interactive terminal and temporary jobs`.

**Expected:** fermer/réouvrir la fenêtre puis sélectionner un serveur reconstruit le terminal.

### Task 15: Resources, Ports et Settings UI

**Files:**

- Create: `src/features/resources/*`
- Create: `src/features/ports/*`
- Create: `src/features/settings/*`
- Create: `src/features/agent-setup/*`
- Create: tests colocalisés et `tests/e2e/dashboard.spec.ts`

- [ ] Construire cartes, diagnostics, historiques, tables accessibles.
- [ ] Construire ports groupés, refresh et confirmations destructives.
- [ ] Construire settings General/Servers/Memory/Privacy.
- [ ] Construire agent setup en deux étapes et feedback update.
- [ ] Vérifier erreurs partielles, responsive minimum et contraste AA.
- [ ] Commit: `feat: complete resource and settings dashboards`.

**Expected:** chaque écran de `docs/06-ui-react.md` est couvert par test et état d’erreur.

### Task 16: Installateur, signatures, CI et release dry-run

**Files:**

- Create: `scripts/build.ps1`, `scripts/package-cli.ps1`, `scripts/verify-release.ps1`
- Create: `.github/workflows/ci.yml`, `.github/workflows/release.yml`
- Modify: `src-tauri/tauri.conf.json`
- Create: `LICENSES/Portly-MIT.txt`, `THIRD_PARTY_NOTICES.md`
- Create: `tests/e2e/installer.ps1`, `tests/e2e/update-handoff.ps1`

- [ ] Configurer NSIS user-mode, CLI/skill/resources et WebView2 Evergreen.
- [ ] Tester install, PATH, autostart, upgrade, uninstall et conservation config.
- [ ] Configurer updater artifacts/signature et Authenticode avec timestamp.
- [ ] Implémenter CI complète et verification post-download.
- [ ] Faire un dry-run signé sur release privée et vérifier tous les hashes.
- [ ] Exécuter la matrice de `docs/08-tests-validation.md`.
- [ ] Commit: `build: add signed Windows release pipeline`.

**Expected:** un compte Windows non-admin installe, utilise, met à jour et désinstalle NetsuStack sans processus/port orphelin.

## Vérification finale du plan

- La spécification fonctionnelle est couverte de Task 2 à Task 16.
- Les écarts Windows sont traités dans Tasks 5, 8 et 9.
- API/CLI, UI et runtime partagent le même `AppService`.
- Aucun champ, route, écran ou test de parité n’est repoussé hors du plan.
- ARM64, Store et service système restent explicitement hors v1.
