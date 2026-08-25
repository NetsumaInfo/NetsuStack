# Audit du dépôt Portly

## 1. Référence et méthode

- Dépôt : <https://github.com/Melvynx/portly>
- Branche : `main`
- Commit analysé : `ed0e1b7` (`docs: add the Linux CLI page to the website`)
- Date du commit : 22 août 2026
- Licence : MIT, copyright 2026 Melvynx. Toute réutilisation substantielle doit conserver la notice MIT.
- Taille observée : 30 commits, 29 302 lignes suivies, dont 51 fichiers Swift, 24 Go, 11 TSX et 4 TypeScript.

Le dépôt contient quatre produits distincts :

1. `Sources/PortlyApp` : application native macOS SwiftUI/AppKit, superviseur principal.
2. `Sources/PortlyCLI` + `Sources/PortlyCore` : CLI macOS et contrats partagés.
3. `cli/` : superviseur headless Linux autonome en Go, reproduisant l’API et la CLI.
4. `website/` et `StoreApp/` : site marketing React et compagnon Mac App Store sandboxé.

Pour NetsuStack, les deux sources normatives sont l’app Swift pour l’expérience complète et le CLI Go pour la séparation headless. En cas de divergence, le comportement macOS documenté et testé prime.

## 2. Capacités produit observées

- Projets persistants contenant plusieurs serveurs.
- Serveurs démarrés dans un vrai PTY, avec terminal interactif et scrollback conservé.
- États `stopped`, `starting`, `running`, `unhealthy`, `restarting`, `failed`.
- Vérification TCP ou HTTP, redémarrage après trois échecs de santé.
- Redémarrage automatique exponentiel : 1, 2, 4, 8, 16 puis 30 secondes, limité par `maxRestartAttempts`.
- Arrêt de tout le groupe de processus ; escalade forcée après 5 secondes pour les processus gérés.
- Jobs temporaires non persistés, timeout maximal de 7 jours, résultat conservé une heure.
- Actions de maintenance exécutées comme jobs temporaires sans arrêter le serveur principal.
- Inspection des ports TCP IPv4/IPv6 et identification du processus propriétaire.
- Prise de contrôle explicite d’un port externe, jamais automatique.
- Détection Docker : arrêt du conteneur publiant le port, pas du backend Docker Desktop.
- Logs texte sans séquences ANSI, mémoire circulaire et fichier rotatif.
- Terminal ANSI coloré séparé des logs texte.
- Échantillonnage CPU/mémoire toutes les 2 secondes, historique de 5 minutes.
- Limites mémoire globale et par projet ; redémarrage après trois échantillons consécutifs au-dessus du seuil.
- Diagnostics de forte consommation, croissance soutenue et sessions de développement dupliquées.
- Application accessible depuis la fenêtre et la barre de menus ; fermer la fenêtre ne quitte pas le superviseur.
- API HTTP sur `127.0.0.1:7737` et CLI qui lance l’application si nécessaire.
- Démarrage à la connexion, installation du CLI et d’un skill agent.
- Mises à jour signées Sparkle avec reprise des serveurs actifs après relance.

## 3. Carte du code Swift

### PortlyCore

| Fichier | Responsabilité à reproduire |
| --- | --- |
| `API.swift` | Enveloppe JSON, requêtes/réponses HTTP, encodeur ISO-8601. |
| `APIClient.swift` | Client loopback, lancement de l’app si absente, erreurs réseau/API. |
| `ConfigStore.swift` | Lecture/écriture atomique de `config.json`, watch avec debounce, hot reload. |
| `Models.swift` | Modèles persistés et runtime, valeurs par défaut, résolution des noms/IDs, formats mémoire et timeout. |
| `Paths.swift` | Emplacements config et logs. |
| `Version.swift` | Version produit unique. |

### PortlyApp — runtime et système

| Fichier | Responsabilité à reproduire |
| --- | --- |
| `PortlyApp.swift` | Cycle de vie, fenêtre, menu bar, API locale, arrêt global à la vraie sortie. |
| `Supervisor.swift` | Source de vérité, runtimes, jobs temporaires, métriques, historique, limites mémoire, reprise après update. |
| `ServerRuntime.swift` | PTY, états, santé, timeout, logs, arrêt, reprise, backoff, takeover. |
| `ControlServer.swift` | Serveur HTTP loopback, routage, validation, enveloppes JSON. |
| `HealthChecker.swift` | Probes TCP localhost et HTTP 2xx/3xx ou statut exact. |
| `LogStore.swift` | Ring buffer, nettoyage ANSI/VT, CR, rotation `.1.log`. |
| `ProcessMetrics.swift` | Arbres de processus gérés, CPU, footprint, RSS, processus externes. |
| `MemoryLimitGuard.swift` | Trois dépassements consécutifs avant redémarrage. |
| `MemoryDiagnostics.swift` | Seuils adaptés à la machine, croissance par médianes, conseils ciblés. |
| `PortInspector.swift` | Listeners TCP, PID, utilisateur, cwd, arrêt sécurisé, déduplication IPv4/IPv6. |
| `DockerPortInspector.swift` | `docker ps`/`inspect`/`stop` sur le conteneur exact. |
| `ExternalProcessController.swift` | Validation anti-réutilisation de PID et arrêt d’un arbre externe. |
| `CommandDetector.swift` | Détection Node/monorepo/Rust/Go/Django/Rails/Compose/Procfile et ports. |
| `Notifications.swift` | Notification d’échec d’un serveur. |
| `Updater.swift` | Pont Sparkle et préparation de la reprise. |
| `UpdaterRelaunchStateStore.swift` | Marqueur de reprise consommable une seule fois et expirant. |
| `Analytics.swift` | Télémétrie minimale et allowlistée ; optionnelle pour NetsuStack. |
| `AppPresentation.swift` | Garantie d’au moins un point d’entrée Dock/menu bar. |

### PortlyApp — interface

| Fichier | Responsabilité à reproduire |
| --- | --- |
| `MainView.swift` | Navigation principale, recherche, projets, temporaire, ressources, ports, menus contextuels. |
| `ServerDetail.swift` | Statut, commandes, actions, terminal, métriques et conflit de port. |
| `TerminalPane.swift` | Surface terminal persistante, palette, focus et redimensionnement. |
| `Forms.swift` | Ajout/édition projet/serveur, auto-détection, job temporaire, limite mémoire. |
| `ResourceDashboard.swift` | Résumé machine, diagnostics, graphiques, processus gérés/externes. |
| `PortsView.swift` | Inventaire des listeners, regroupement, ouverture URL et arrêt confirmé. |
| `SettingsView.swift` | Général, supervision/logs, mémoire, update et agent setup. |
| `MenuBarContent.swift` | Contrôle rapide des serveurs, navigation, stop all, quit. |
| `MenuBarIcon.swift` | Icône inactive/active. |
| `SidebarSearch.swift` | Recherche projet, dossier, serveur, port, localhost et commande. |
| `AgentSetup.swift` | Installation idempotente du skill, CLI et règles globales. |
| `UIComponents.swift` | Typographie, couleurs, badges et bouton start/stop. |
| `Motion.swift` | Mouvement respectant la réduction d’animations. |

## 4. Carte du superviseur Linux Go

| Fichier | Responsabilité | Usage pour Windows |
| --- | --- | --- |
| `main.go`, `commands.go`, `render.go` | Parsing CLI, aliases, sorties humaines/JSON. | Contrat comportemental direct du futur `netsustack.exe`. |
| `api.go`, `client.go`, `jsonutil.go` | API loopback, middleware, client, enveloppes. | Modèle du serveur Axum et du client Rust. |
| `models.go`, `config.go`, `paths.go` | Données, compatibilité ascendante, config, chemins. | Réutiliser les mêmes valeurs par défaut et migrations. |
| `supervisor.go`, `runtime.go` | Orchestration, états, jobs, limites, takeover. | Oracle headless pour les tests de parité. |
| `process.go` | PTY/pipes, shell, groupe de processus, environnement. | À remplacer par ConPTY + Job Object. |
| `ports.go` | `/proc`, `ss`, `lsof`. | À remplacer par IP Helper API. |
| `metrics.go`, `memory.go` | `/proc`, CPU/RSS et garde mémoire. | À remplacer par Win32 process/job APIs. |
| `docker.go` | Résolution d’un port publié. | Logique largement portable via Docker CLI. |
| `health.go` | TCP/HTTP. | Portable via Tokio/Reqwest. |
| `logs.go`, `timeout.go` | Logs et durées. | Portable presque directement. |
| `forever.go`, `daemon*.go` | systemd user et daemon auto-démarré. | À remplacer par autostart Tauri + instance unique. |
| `core_test.go` | Tests de contrats et intégrations principales. | À porter en priorité comme tests Rust. |

## 5. Site, StoreApp et scripts

- `website/` est une référence de contenu et d’apparence, pas une base du renderer Tauri. `website/public/screenshots/portly-dashboard.png` montre la composition historique terminal/sidebar.
- `StoreApp/` est un compagnon sandboxé qui ne supervise rien. Aucun équivalent Windows Store n’est nécessaire pour la première version ; une version ultérieure pourra consommer l’API locale.
- `build.sh` assemble l’app, le CLI et le skill, préserve les serveurs actifs pendant la réinstallation et ajoute une règle agent idempotente.
- `release.sh` construit exactement le commit poussé, signe/notarise et publie les artefacts.
- `.github/workflows/ci.yml` valide l’app macOS, le CLI Linux amd64/arm64 et le site.

## 6. Tests source à porter

Le dépôt teste explicitement :

- parsing timeout et mémoire ;
- seuil de trois échantillons ;
- décodage des anciennes configurations ;
- codes de sortie des jobs temporaires ;
- nettoyage ANSI/CR ;
- bind loopback exclusif ;
- cycle temp/wait et cycle projet/serveur ;
- présentation fenêtre/menu bar ;
- reprise update consommée une fois et expirante ;
- sélection de couleurs et graphiques ;
- diagnostics mémoire et pics isolés ;
- refus d’arrêt lors d’une réutilisation de PID ;
- Docker sur le port exact ;
- recherche sidebar et layout du champ de recherche.

## 7. Divergences et pièges constatés

1. La version macOS et le daemon Go dupliquent une partie importante des modèles et du runtime. NetsuStack doit partager un seul cœur Rust entre app, API et CLI.
2. Le `physical footprint` macOS n’a pas d’équivalent Windows exact. NetsuStack doit nommer et tester sa métrique Windows au lieu de prétendre à une identité numérique.
3. SwiftTerm conserve l’objet terminal même sans vue. En React, démonter xterm détruirait cet état ; le runtime Rust doit conserver un transcript ANSI borné et le rejouer à l’attachement.
4. Le cwd d’un processus externe est facile via `lsof` sur macOS mais n’est pas une donnée Win32 fiable sans lecture fragile du PEB. La parité Windows doit afficher « inconnu » ou une inférence explicite.
5. `SIGTERM`/`SIGKILL` n’existent pas sous la même forme. L’arrêt géré doit utiliser Ctrl+C/ConPTY puis Job Object ; l’arrêt externe doit être décrit honnêtement comme terminaison.
6. Une API loopback sans authentification reste accessible aux autres processus et peut être ciblée depuis un navigateur. NetsuStack ajoute un jeton local sans modifier les formes JSON.
