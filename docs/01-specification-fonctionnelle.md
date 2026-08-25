# Spécification fonctionnelle NetsuStack

## 1. Promesse produit

NetsuStack supervise tous les serveurs de développement locaux d’un utilisateur Windows depuis une seule application. Chaque commande s’exécute dans un terminal interactif réel, garde son port et son état de santé, redémarre après incident selon une politique bornée et reste contrôlable depuis la fenêtre, la zone de notification, le CLI et une API locale.

## 2. Acteurs

- Développeur : configure et observe projets, serveurs, ports, logs et ressources.
- Agent de code : utilise exclusivement le CLI JSON ou humain pour lancer et vérifier les workloads.
- Processus supervisé : commande persistante ou job temporaire exécuté avec l’environnement prévu.
- Processus externe : listener ou workload non possédé par NetsuStack ; jamais arrêté automatiquement.

## 3. Projets persistants

Un projet possède un ID stable, un nom unique insensible à la casse, un chemin racine absolu, une icône, une couleur, une politique mémoire et zéro ou plusieurs serveurs. Les projets persistent dans `config.json`. Leur suppression arrête d’abord tous leurs serveurs.

Critères :

- ajout, édition, suppression et sélection depuis l’UI et le CLI ;
- validation de l’existence du dossier ;
- résolution par ID ou nom ;
- recherche par nom, dossier, serveur, port, `localhost:<port>` ou commande ;
- start all et stop all ;
- aucune suppression silencieuse d’un workload actif.

## 4. Serveurs persistants

Un serveur contient nom, commande, port optionnel, répertoire relatif ou absolu, environnement, health URL/statut, redémarrage automatique et actions.

Au démarrage :

1. refuser si le serveur est déjà actif ;
2. refuser si le dossier n’existe pas ;
3. refuser si le port configuré est occupé ;
4. créer ConPTY et Job Object ;
5. injecter l’environnement ;
6. passer à `starting` ;
7. lancer les sondes ;
8. passer à `running` après la première santé réussie.

Un serveur sans port ni health URL est sain tant que son processus vit.

## 5. États et transitions

| État | Signification | Sorties autorisées |
| --- | --- | --- |
| `stopped` | Aucun processus actif, arrêt normal ou manuel. | `starting` |
| `starting` | Processus lancé, santé pas encore confirmée. | `running`, `failed`, `stopped` |
| `running` | Processus vivant et sain. | `unhealthy`, `restarting`, `stopped`, `failed` |
| `unhealthy` | Processus vivant, sonde en échec. | `running`, `restarting`, `stopped` |
| `restarting` | Backoff automatique en cours. | `starting`, `stopped`, `failed` |
| `failed` | Démarrage impossible ou budget épuisé. | `starting`, `stopped` |

Trois échecs de santé consécutifs déclenchent un restart si `autoRestart=true`. Un crash déclenche un backoff 1/2/4/8/16/30 s. Après plus de 30 s en bonne santé, le budget est remis à zéro. Un start/restart manuel remet aussi le compteur à zéro.

## 6. Terminal et logs

- Terminal interactif UTF-8/VT via ConPTY.
- Entrée clavier, Ctrl+C, redimensionnement, sélection et copie.
- Palette sombre équivalente à Portly, police Geist Mono puis Consolas/Cascadia Mono.
- Le terminal survit au changement de serveur et se reconstruit après fermeture/réouverture de la fenêtre grâce au transcript ANSI.
- Les logs API sont du texte nettoyé : suppression ANSI/OSC, gestion CR/CRLF, limite mémoire configurable, rotation à `logFileMaxMB` vers `.1.log`.
- Les notes NetsuStack (`starting`, `stopping`, `restart`, `exit`, timeout) sont intégrées au flux.

## 7. Santé

- Sonde rapide chaque seconde jusqu’au premier état stable, puis `healthIntervalSeconds` avec minimum 2 s.
- Port seul : connexion TCP à `localhost`, essais IPv4 et IPv6.
- `healthURL` relative : `http://localhost:<port>/<path>`.
- URL absolue : HTTP ou HTTPS.
- `healthStatus` absent : 200–399 ; présent : égalité exacte.
- Timeout TCP 2 s, HTTP 5 s.

## 8. Jobs temporaires

- Hors projet, non écrits dans `config.json`, démarrés immédiatement.
- Nom dérivé des quatre premiers mots de la commande s’il est absent.
- Répertoire courant par défaut, environnement et port optionnels.
- Timeout par défaut 30 min, maximum 7 jours.
- États : `running`, `succeeded`, `failed`, `timedOut`, `stopped`.
- Codes CLI : succès `0`, timeout `124`, arrêt manuel `130`, échec code enfant non nul ou `1`.
- Résultat et logs disponibles une heure après terminaison, puis suppression de la mémoire runtime.
- Jamais restaurés après relance ou update.

## 9. Actions de serveur

Chaque action possède un nom unique insensible à la casse et une commande non vide. Elle s’exécute comme job temporaire avec cwd, env, `PORT` et nom de serveur du serveur parent. Elle ne stoppe ni ne redémarre celui-ci.

## 10. Ports et prise de contrôle

- Afficher tous les listeners TCP IPv4/IPv6 avec port, PID, exécutable, utilisateur et ownership NetsuStack.
- Dédupliquer les doubles binds IPv4/IPv6.
- Ouvrir `http://localhost:<port>` sur action utilisateur.
- Ne jamais arrêter automatiquement un processus externe.
- `take-over` exige un serveur configuré et un port occupé, revalide PID + heure de création, arrête l’occupant ou son conteneur Docker, attend au maximum 10 s, puis lance le serveur configuré.
- Pour Docker, résoudre le conteneur qui publie exactement le port et exécuter `docker stop --time 10 <id>`.

## 11. Ressources et mémoire

- Échantillon toutes les 2 s.
- Historique global et par projet : 150 points, soit 5 minutes.
- CPU agrégé de l’arbre, où un cœur pleinement occupé vaut 100 %.
- Mémoire possédée Windows : somme de `PrivateUsage` des processus actifs.
- RAM résidente : somme de `WorkingSetSize`.
- Jusqu’à 30 processus externes de l’utilisateur, minimum 16 MiB de working set ; détails enrichis toutes les 10 s.
- Limite globale optionnelle, appliquée séparément à chaque projet.
- Projet : `inherit`, `disabled` ou `custom` entre 128 MiB et 1 TiB.
- Trois échantillons consécutifs au-dessus de la limite redémarrent ensemble les serveurs actifs du projet.
- Diagnostics : serveur lourd, enfant lourd, croissance soutenue par médianes, processus externe lourd, sessions Next/Vite/Convex/npm/pnpm dupliquées.

## 12. Présentation et cycle de vie

- Une seule instance de l’application.
- Fermer la fenêtre la masque ; les serveurs continuent.
- L’icône de zone de notification reste disponible si la fenêtre est masquée.
- Le menu tray permet ouverture, start/stop individuel, Resources, Ports, Settings, Stop All et Quit.
- Quit est la seule sortie normale : elle stoppe tous les processus gérés et ferme l’API.
- Le CLI peut démarrer l’application installée et attendre l’API jusqu’à 20 s.
- Les serveurs ne redémarrent pas automatiquement après un boot normal ; seuls les serveurs explicitement actifs pendant un transfert `forever` ou une mise à jour sont repris.

## 13. Auto-détection de commandes

La détection analyse au plus 12 sous-dossiers de `apps`, `packages`, `services` et propose au plus huit scripts principaux : `dev`, `start`, `serve`, `develop`, `watch`, puis les variantes préfixées.

Elle reconnaît :

- lockfiles pnpm, Bun, Yarn, npm ;
- ports `--port`, `-p`, `PORT=`, `.env.local`, `.env.development`, `.env` ;
- Next 3000, Nuxt 3000, Remix 3000, Vite/SvelteKit 5173, Astro 4321, Expo 8081, Angular 4200, Convex 3210 ;
- `Cargo.toml`, `go.mod`, `manage.py`, Rails, Compose et Procfile.

Un port déjà réservé est remplacé par le prochain port libre ; l’argument `-- --port` n’est ajouté que pour un framework qui le supporte.

## 14. Installation agent

- Installer le skill sous `%USERPROFILE%\.agents\skills\netsustack`.
- Mettre à jour les blocs balisés dans `%USERPROFILE%\.agents\AGENTS.md` et `%USERPROFILE%\.claude\CLAUDE.md` sans écraser le contenu existant.
- Installer le CLI et vérifier qu’il est accessible sur `PATH`.
- Répéter l’opération doit être idempotent.

## 15. Hors périmètre initial

- Support macOS/Linux dans le binaire Tauri NetsuStack.
- Service Windows système et exécution avant connexion utilisateur.
- Mode administrateur permanent.
- Supervision distante ou bind réseau.
- Cloud, compte utilisateur et synchronisation.
- Package Microsoft Store compagnon.
- ARM64 comme artefact de sortie v1 ; le code ne doit toutefois pas dépendre de x86-64 sans isolation.
