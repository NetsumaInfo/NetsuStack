# Contrats de données, API et CLI

## 1. Persistance

Chemins Windows :

```text
%USERPROFILE%\.config\netsustack\config.json
%USERPROFILE%\.config\netsustack\logs\<server-id>.log
%USERPROFILE%\.config\netsustack\logs\<server-id>.1.log
%USERPROFILE%\.config\netsustack\api-token
%USERPROFILE%\.config\netsustack\resume-after-update.json
```

`config.json` est la seule source persistante de projets/serveurs. Les préférences purement UI peuvent utiliser le store Tauri. Les jobs temporaires, métriques et états runtime ne sont pas persistés.

## 2. Schéma de configuration

```json
{
  "version": 1,
  "apiPort": 7737,
  "healthIntervalSeconds": 10,
  "maxRestartAttempts": 5,
  "logBufferLines": 5000,
  "logFileMaxMB": 10,
  "globalMemoryLimitBytes": null,
  "preferredShell": "auto",
  "projects": [
    {
      "id": "prj_a1b2c3d4",
      "name": "NetsuStack",
      "icon": "shippingbox.fill",
      "color": "#0A84FF",
      "root": "S:\\projet_app\\NetsuStack",
      "memoryLimitMode": "inherit",
      "memoryLimitBytes": null,
      "servers": [
        {
          "id": "srv_a1b2c3d4",
          "name": "web",
          "command": "npm run dev",
          "port": 5173,
          "directory": null,
          "env": {},
          "healthURL": null,
          "healthStatus": null,
          "autoRestart": true,
          "actions": [
            { "name": "clear-cache", "command": "powershell -NoProfile -Command Remove-Item -Recurse -Force .\\node_modules\\.vite" }
          ]
        }
      ]
    }
  ]
}
```

Compatibilité ascendante : tous les champs ajoutés après `version`, `apiPort` et `projects` ont des valeurs par défaut. Un champ inconnu est ignoré. Une migration écrit d’abord un backup horodaté, puis remplace atomiquement le fichier.

## 3. Modèles runtime

### `ServerStatus`

```text
id, name, projectID, projectName, command, port?, directory,
state, pid?, startedAt?, restartCount, lastExitCode?, lastError?, healthy,
url?, cpuPercent?, memoryBytes?, residentMemoryBytes?, processCount?,
temporary?, timeoutSeconds?, deadline?, finishedAt?, timedOut?
```

### `ProjectStatus`

```text
id, name, icon, color, root, servers,
memoryLimitMode, memoryLimitBytes?, effectiveMemoryLimitBytes?,
lastMemoryRestartAt?, lastMemoryRestartBytes?
```

### `NetsuStackStatus`

```text
version, apiPort, revision, globalMemoryLimitBytes?, projects, temporaryServers
```

Dates : RFC 3339/ISO-8601 UTC. Tailles : octets entiers non signés. PID : entier 32 bits. Les noms JSON restent en camelCase.

## 4. API locale

Base : `http://127.0.0.1:<apiPort>`. Le serveur n’écoute jamais sur `0.0.0.0`, `::` ou une interface LAN.

Enveloppe :

```json
{ "ok": true, "data": {}, "error": null }
```

ou :

```json
{ "ok": false, "data": null, "error": "message stable et actionnable" }
```

Toutes les routes sauf `GET /ping` exigent `X-NetsuStack-Token`. Le token est généré au premier lancement, lisible seulement par l’utilisateur courant. Les requêtes ayant un header `Origin` autre que l’origine Tauri autorisée sont refusées.

| Méthode | Route | Corps / query | Résultat |
| --- | --- | --- | --- |
| GET | `/ping` | — | version et disponibilité |
| GET | `/status` | — | snapshot complet |
| GET | `/config` | — | config persistée |
| GET | `/logs` | `server`, `tail` | lignes nettoyées |
| GET | `/temporary/status` | `id` | job temporaire |
| GET | `/ports` | `port` | occupant du port |
| POST | `/start`, `/stop`, `/restart` | `server?`, `project?` | IDs affectés |
| POST | `/temporary/run` | job temporaire | statut initial |
| POST | `/actions/run` | serveur/action/timeout | statut initial |
| POST | `/memory-limit` | projet?/mode/bytes? | politique appliquée |
| POST | `/projects/add`, `/projects/remove` | projet | projet/action |
| POST | `/servers/add`, `/servers/update`, `/servers/remove` | serveur | serveur/action |
| POST | `/servers/take-over` | serveur | action |
| POST | `/ports/kill` | port + identité observée | action |
| POST | `/open` | destination? | action |
| POST | `/quit` | objet vide | action puis arrêt |

Les codes HTTP sont 200 succès, 400 validation/conflit, 401 token, 403 origin, 404 cible, 405 méthode, 500 incident interne. Même en erreur HTTP, le corps reste une enveloppe JSON.

## 5. Résolution des cibles

Serveur : ID exact, `project/server` insensible à la casse, puis premier nom de serveur insensible à la casse. Le CLI recommande `project/server` si plusieurs noms correspondent. Projet : ID exact puis nom insensible à la casse.

`start`/`restart` sans cible est refusé. `stop --all` est explicite. L’API ne considère l’absence de cible comme `all` que pour `/stop` afin d’éviter une action globale accidentelle.

## 6. CLI normatif

```text
netsustack status [--details] [--json]
netsustack start <server> | --project <project>
netsustack stop <server> | --project <project> | --all
netsustack restart <server> | --project <project>
netsustack action <server> <action> [--timeout 30m]
netsustack logs <server> [--tail 200]
netsustack temp '<command>' [--name] [--path] [--port] [--health-url] [--timeout] [--env KEY=VALUE]
netsustack wait <job-id> [--tail 500] [--no-logs]
netsustack add-project --name --path [--icon] [--color] [--memory-limit]
netsustack add-server --project --name --command [--port] [--directory] [--health-url] [--env] [--action] [--start]
netsustack update-server <server> [fields] [--action] [--clear-actions]
netsustack memory-limit [SIZE|off|inherit] [--project]
netsustack remove <server> | --project <project>
netsustack take-over <server>
netsustack port <port>
netsustack kill-port <port>
netsustack open [--resources|--ports]
netsustack quit
netsustack forever enable|status|disable
netsustack config [--path-only]
```

Aliases de parité : `status|list|ls`, `temp|temporary|run-temp`, `memory-limit|ram-limit`, `take-over|adopt`.

Flags globaux : `--json`, `--api-port`. `--json` garantit un seul document JSON sur stdout. Les erreurs ont un message sur stderr et un exit code non nul.

## 7. Environnement enfant

Le processus hérite de l’environnement de NetsuStack, puis reçoit :

```text
TERM=xterm-256color
COLORTERM=truecolor
FORCE_COLOR=1
CLICOLOR=1
CLICOLOR_FORCE=1
TERM_PROGRAM=NetsuStack
NETSUSTACK=1
NETSUSTACK_SERVER=<nom>
PORT=<port si configuré>
```

Les variables serveur remplacent l’héritage. `NO_COLOR` est supprimé. Pour une compatibilité agent temporaire, `PORTLY=1` peut être proposé par option d’import, mais n’est pas injecté par défaut.

## 8. Parsing

- Timeout : nombre de secondes ou suffixe `s`, `m`, `h`; arrondi supérieur ; 1 s à 604 800 s.
- Mémoire : virgule ou point ; espaces ignorés ; `MB/MiB/Mo`, `GB/GiB/Go`, `TB/TiB/To`; plage 128 MiB–1 TiB.
- Action : `NAME=COMMAND`, nom/commande non vides, noms uniques sans casse.
- Port : 1–65 535 ; `apiPort` ne peut pas être réutilisé par un serveur.

## 9. Skill agent NetsuStack

Le skill doit enseigner : inspecter `status` d’abord, distinguer permanent/temporaire, ne jamais lancer un serveur persistant en arrière-plan hors NetsuStack, utiliser `--json` uniquement pour un traitement structuré, vérifier séparément état/log/port/URL et demander autorisation avant kill/takeover/memory-limit.
