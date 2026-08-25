# Spécification UI React/TypeScript

## 1. Direction

L’interface conserve la densité et la hiérarchie de Portly, mais utilise les conventions Windows plutôt qu’un faux habillage macOS. Elle reste sombre par défaut, compacte, lisible et orientée terminal. La parité porte sur les informations et actions, pas sur les boutons rouge/jaune/vert d’AppKit.

Fenêtre : 1080×660 par défaut, minimum 900×560, état/position restaurés. Une fermeture masque la fenêtre. `Alt+F4` suit le même comportement ; Quit est explicite dans le tray/menu.

## 2. Navigation principale

Disposition à deux colonnes :

- sidebar 240–300 px redimensionnable ;
- contenu flexible, terminal jamais inférieur à 480 px ;
- drag region Tauri dans la barre supérieure ;
- contrôles fenêtre Windows natifs ou équivalent Tauri accessible.

Ordre sidebar :

1. onboarding agent tant qu’incomplet et non masqué ;
2. recherche ;
3. `Temporary` avec jobs actifs/récents ;
4. projets et serveurs ;
5. `Resources` ;
6. `Ports` ;
7. Run Temporary et Add Project.

La sélection est une union discriminée `project | server | temporary | resources | ports`. Si l’élément sélectionné disparaît, sélectionner le premier résultat visible, sinon l’état vide.

## 3. Écrans

### Projet

- icône/couleur, nom, chemin ;
- start all, stop all, add server, edit ;
- liste serveurs avec état, port, CPU, mémoire et action start/stop ;
- empty state unique invitant à détecter ou ajouter un serveur.

### Serveur

- header : badge état, PID, port, uptime, restarts ;
- toolbar : start/stop, restart, actions, open URL, clear, edit ;
- conflit de port : occupant, Docker éventuel, Terminate et Move to NetsuStack ;
- panneau de métriques repliable ;
- terminal xterm occupant le reste.

### Temporary

- même détail terminal ;
- timeout/deadline/elapsed/exit code visibles ;
- actions Stop, Run Again, Remove ;
- succès, timeout et échec visuellement distincts sans dépendre uniquement de la couleur.

### Resources

- “Live, updated every 2 seconds” ;
- mémoire physique machine, mémoire privée gérée, RAM résidente, CPU ;
- cartes diagnostics avec action seulement pour un runtime géré ;
- historique global 5 min ;
- historique par projet ;
- barres par projet ;
- table processus gérés ;
- jusqu’à 30 processus externes, bouton Terminate confirmé, Move to NetsuStack seulement avec listener exploitable.

### Ports

- refresh explicite et timestamp ;
- groupes NetsuStack, applications utilisateur, Docker, système/protégé ;
- port, application, PID, user, chemin, projet/serveur ;
- ouvrir localhost si pertinent ;
- confirmation avant Terminate/Stop container ;
- erreur partielle affichée sans supprimer la dernière liste valide.

### Settings

Onglets :

- General : launch at login, close to tray, shell, config, CLI/agent setup, update, version ;
- Servers : interval santé, restart attempts, lignes/mégaoctets de logs ;
- Memory : garde globale et overrides projets, explication des trois échantillons ;
- Privacy : analytics désactivée par défaut, diagnostic local uniquement.

## 4. Formulaires

### Projet

Nom, dossier choisi par dialog natif, icône, couleur, politique mémoire. Validation inline et au submit. Le bouton principal est désactivé seulement si la requête est en vol ou les champs structurels sont invalides.

### Serveur

Deux modes : detected et manual. Les suggestions affichent source, commande, cwd relatif et port. Champs manuels : nom, commande, port, dossier, health URL, health status, auto restart, env et actions.

### Temporaire

Nom facultatif, commande obligatoire, dossier, port, health URL, timeout et env. Valeur timeout initiale : `30m`.

### Mémoire

Global : off/custom. Projet : inherit/off/custom. Valeurs saisies en texte humain, preview normalisé, plage 128 MiB–1 TiB.

## 5. Recherche et clavier

- `Ctrl+K` ou `/` focalise la recherche si le terminal n’a pas le focus.
- `Escape` vide puis quitte la recherche.
- Flèches naviguent les résultats ; Enter sélectionne.
- `Ctrl+L` focalise le terminal/log si un serveur est sélectionné.
- Aucun raccourci global ne capture Ctrl+C du terminal.
- Tous les boutons icon-only ont `aria-label` et tooltip différé.

## 6. Terminal

Palette source :

```text
background #0B0D12    foreground #D6DBE6    border #333B4A
red #FF6B81           green #A7D46F         yellow #F5C76D
blue #82AAFF          magenta #C792EA       cyan #63D4D5
```

Police : Geist Mono embarquée si sa licence est conservée, puis Cascadia Mono, Consolas, monospace. Taille 13 px. Insets : surface 10 px, texte 12 px, rayon 10 px.

Le lien local est cliquable uniquement pour `http://localhost`, `http://127.0.0.1` et `https` explicite. Tout autre lien demande confirmation avant ouverture externe.

## 7. Icônes

Les identifiants Portly restent acceptés dans les imports de config mais sont mappés vers Fluent :

| Portly | NetsuStack |
| --- | --- |
| `shippingbox.fill` | Box |
| `cube.fill` | Cube |
| `globe` | Globe |
| `server.rack` | Server |
| `bolt.fill` | Flash |
| `cloud.fill` | Cloud |
| `hammer.fill` | Hammer |
| `flask.fill` | Beaker |
| `cart.fill` | Cart |
| `envelope.fill` | Mail |
| `chart.bar.fill` | DataBarVertical |
| `star.fill` | Star |
| `heart.fill` | Heart |
| `gamecontroller.fill` | Games |
| `camera.fill` | Camera |
| `music.note` | MusicNote |
| `book.fill` | Book |
| `terminal.fill` | WindowConsole |

Les nouvelles configs écrivent des IDs sémantiques NetsuStack ; l’import conserve la compatibilité.

## 8. États UI obligatoires

- démarrage application ;
- snapshot indisponible avec retry ;
- aucun projet ;
- projet sans serveur ;
- serveur starting/unhealthy/restarting/failed ;
- terminal détaché/replay/live ;
- liste ports partielle ;
- métriques permission denied ;
- Docker absent ;
- update disponible/téléchargement/restart ;
- agent setup partiel/complet/erreur.

Une erreur ne remplace jamais toute la page si une donnée valide précédente existe.

## 9. Accessibilité et mouvement

- Contraste AA, focus visible, navigation entièrement clavier.
- Les graphiques ont un résumé textuel et une table accessible.
- États doublés par texte/symbole, pas couleur seule.
- `prefers-reduced-motion` supprime les transitions non essentielles.
- Les annonces live de logs sont désactivées ; seules les transitions d’état sont annoncées.

## 10. Composants cibles

```text
AppShell, TitleBar, Sidebar, SidebarSearch, ProjectTree, TemporaryTree,
DestinationButton, ProjectPage, ServerPage, TerminalSurface, StatusBadge,
ServerControls, PortConflictCard, ResourceDashboard, ResourceChart,
ProcessTable, PortsPage, ProjectForm, ServerForm, TemporaryForm,
MemoryLimitEditor, SettingsDialog, AgentSetupCard, ConfirmDialog, ToastRegion
```

Chaque composant de feature reçoit des DTO et callbacks typés ; il n’appelle pas directement `invoke` sauf via le client de feature.
