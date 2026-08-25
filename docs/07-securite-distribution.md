# Sécurité, installation et mises à jour

## 1. Modèle de menace

NetsuStack exécute des commandes arbitraires choisies par l’utilisateur et peut terminer des processus. Les frontières sont : contenu WebView, API loopback, fichiers de config, CLI/agents, processus externes, Docker et chaîne de mise à jour.

Menaces prioritaires :

- page web hostile appelant localhost ;
- XSS obtenant des capacités shell/filesystem ;
- config modifiée pour exécuter une commande au prochain start ;
- PID réutilisé entre affichage et arrêt ;
- update non authentique ;
- DLL hijacking ou CLI remplacé ;
- chemins/règles agent écrasés au lieu d’être fusionnés.

## 2. Autorité

- L’ajout d’une commande à la config n’exécute rien automatiquement.
- Start, action, takeover, kill, limite mémoire et update demandent une action explicite UI/CLI.
- La reprise après update ne concerne que les IDs actifs enregistrés juste avant l’update et expire rapidement.
- L’UI n’a aucune permission Tauri d’exécution shell générale.
- Le Rust backend exécute seulement les commandes présentes dans une requête métier validée.

## 3. API locale

- Bind IPv4 exact `127.0.0.1`.
- Token aléatoire 256 bits, créé avec CSPRNG, fichier ACL utilisateur courant.
- Token exigé pour status, config, logs et toutes les mutations ; `/ping` ne retourne que version/protocole.
- Comparaison constant-time.
- Refus des origins navigateur ; limite corps JSON 1 MiB ; timeout requête 10 s.
- Pas de CORS permissif, pas de websocket réseau.
- Journaliser route/code/durée, jamais token, env complet ni commande secrète.

## 4. Tauri

- CSP : `default-src 'self'`; scripts/styles locaux ; connexions uniquement protocole Tauri ; images `self data:`.
- Pas de remote URL dans le WebView.
- Capabilities ciblées sur `main`, minimum nécessaire.
- Les dialogs/opener sont scindés ; aucun accès `fs:default` ou `shell:default` global.
- `single-instance` est initialisé en premier ; une seconde instance ne démarre aucun runtime et focalise la première.

## 5. Fichiers et chemins

- Config et logs par utilisateur, jamais Program Files.
- Écriture atomique et backup avant migration.
- Projet/cwd canonicalisés ; les liens/junctions sont autorisés mais le chemin final est montré.
- Les actions de révélation utilisent l’opener sur un chemin déjà validé.
- Le skill et les règles agent utilisent staging + rename et blocs balisés idempotents.
- Le token n’est ni exporté dans les enfants ni inclus dans les rapports.

## 6. Processus externes

- Snapshot : PID + creation time + executable + port.
- Revalidation complète avant Terminate/Takeover.
- Processus d’un autre utilisateur : information limitée, arrêt refusé sans élévation ; NetsuStack ne relance pas en admin.
- Processus système/protégés : badge protégé, aucun bouton d’arrêt.
- Docker : arrêt par ID de conteneur revalidé ; jamais le backend global.

## 7. Installation Windows

Artefact principal : NSIS par utilisateur, x86-64.

Contenu :

- `NetsuStack.exe` Tauri ;
- `netsustack.exe` CLI signé ;
- skill agent ;
- fonts/icônes/licences ;
- WebView2 bootstrap seulement si l’Evergreen Runtime manque.

Installation cible : `%LOCALAPPDATA%\Programs\NetsuStack`. Le CLI est copié dans `bin` et l’installateur ajoute ce dossier au PATH utilisateur de façon idempotente. Aucun UAC n’est requis pour l’installation normale.

L’uninstall :

1. demande à l’instance active de quitter ;
2. vérifie que les Job Objects sont terminés ;
3. retire app, CLI, PATH et autostart ;
4. conserve config/logs par défaut avec option explicite de suppression.

Référence : [Tauri Windows Installer](https://v2.tauri.app/distribute/windows-installer/).

## 8. Signatures

Deux mécanismes indépendants :

- Authenticode signe EXE/NSIS/MSI pour Windows/SmartScreen ;
- la clé privée updater Tauri signe l’artefact `.sig` pour l’authenticité de l’update.

Les deux sont requis pour une release publique. La clé updater privée reste uniquement dans le secret CI. Le certificat Authenticode utilise timestamp RFC 3161. Référence : [Windows Code Signing](https://v2.tauri.app/distribute/sign/windows/).

## 9. Updater

`tauri-plugin-updater`, endpoint HTTPS ou `latest.json` GitHub Release, `createUpdaterArtifacts=true`, mode Windows `passive`.

Contrat de reprise :

1. capturer les serveurs persistants actifs ;
2. écrire `{version, createdAt, serverIDs}` atomiquement ;
3. arrêter proprement tous les runtimes ;
4. installer/restart ;
5. consommer le marqueur une seule fois s’il a moins de 10 min ;
6. attendre que chaque port soit libre ;
7. redémarrer les IDs encore présents dans la config ;
8. effacer le marqueur même si certains échouent.

Le manifeste et la signature doivent pointer vers exactement le même installer publié. Référence : [Tauri Updater](https://v2.tauri.app/plugin/updater/).

## 10. Démarrage automatique

`tauri-plugin-autostart` en contexte utilisateur. `forever enable` enregistre l’app et laisse l’instance actuelle vivre ; `disable` retire l’enregistrement sans arrêter les serveurs. Le statut distingue `enabled` et `appRunning`.

Au login, l’app démarre cachée en tray. Elle ne démarre aucun serveur simplement parce qu’il est configuré. Une reprise explicite valide peut redémarrer les serveurs enregistrés lors d’un handoff.

## 11. CI release

Pipeline Windows :

1. checkout propre du commit/tag ;
2. `npm ci` ;
3. typecheck/lint/tests frontend ;
4. `cargo test --workspace` ;
5. tests Windows integration ;
6. build Tauri release ;
7. Authenticode app + CLI + installer ;
8. vérification des signatures ;
9. génération `.sig` updater et `latest.json` ;
10. upload release ;
11. téléchargement neuf des artefacts et revalidation hash/signatures/manifeste ;
12. smoke install/uninstall sur VM Windows.

La release échoue si un artefact est non signé, si la signature du manifeste diffère, si le tag/version divergent ou si un job CI est rouge.

## 12. Licence et attribution

- Conserver `LICENSES/Portly-MIT.txt` si du code ou des textes substantiels sont adaptés.
- Documenter les licences de xterm, Fluent icons, fonts et crates redistribuées.
- Ne pas utiliser le nom, l’icône ou l’identité visuelle Portly comme marque produit.
