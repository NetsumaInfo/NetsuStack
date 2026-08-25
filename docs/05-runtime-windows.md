# Runtime Windows

## 1. Baseline système

ConPTY est disponible depuis Windows 10 version 1809. NetsuStack cible Windows 10 22H2 et Windows 11, ce qui permet d’en faire la primitive unique du terminal. Référence Microsoft : [CreatePseudoConsole](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole).

## 2. Création d’un serveur

Ordre obligatoire :

1. résoudre et canonicaliser le cwd ;
2. vérifier le port ;
3. créer deux paires de pipes synchrones ;
4. appeler `CreatePseudoConsole(cols, rows, inputRead, outputWrite, 0)` ;
5. créer un Job Object ;
6. activer `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` sans autoriser le breakaway ;
7. préparer `STARTUPINFOEXW` avec `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` ;
8. créer le shell suspendu avec `CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED` ;
9. assigner le processus au Job Object ;
10. reprendre le thread ;
11. lancer les tâches async de lecture, wait et santé ;
12. fermer immédiatement tous les handles qui n’appartiennent plus au parent.

Si l’assignation au Job Object échoue, le processus suspendu est terminé avant tout retour d’erreur. Un serveur ne peut jamais démarrer sans ownership de son arbre.

Les Job Objects permettent de gérer un groupe de processus comme une unité ; les enfants héritent normalement du job et `KILL_ON_JOB_CLOSE` garantit le nettoyage. Référence : [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).

## 3. Choix du shell

`preferredShell` :

- `auto` : PowerShell 7 (`pwsh.exe`) s’il est disponible, sinon `cmd.exe` ;
- `powershell7` : `pwsh.exe -NoLogo -NoProfile -Command <commande>` ;
- `windowsPowershell` : `powershell.exe -NoLogo -NoProfile -Command <commande>` ;
- `cmd` : `cmd.exe /D /S /C <commande>` ;
- chemin personnalisé : exécutable validé et arguments de lancement configurés.

Le mode auto préfère PowerShell 7 pour Unicode et scripts modernes. `cmd.exe` reste le fallback le plus compatible avec les shims `.cmd` npm. Git Bash/WSL ne sont jamais sélectionnés implicitement ; ils peuvent être configurés explicitement.

## 4. Arrêt géré

1. marquer l’arrêt manuel et annuler santé/restart/timeout ;
2. écrire `ETX` (`0x03`, Ctrl+C) dans ConPTY ;
3. attendre jusqu’à 5 s la sortie du job ;
4. appeler `TerminateJobObject(job, 1)` si des processus restent ;
5. attendre la notification de fin ;
6. fermer HPCON, pipes et Job Object ;
7. publier l’état final.

Le restart manuel attend la terminaison complète avant de recréer le runtime. Aucun nouveau processus n’est créé tant que l’ancien port n’est pas libre.

## 5. Inspection et arrêt externe

### Listeners

Appeler `GetExtendedTcpTable` pour `AF_INET` et `AF_INET6` avec les tables OWNER_PID en état LISTEN. Dédupliquer par `(port,pid)`. Pour chaque PID accessible :

- `QueryFullProcessImageNameW` : exécutable ;
- WMI ou Toolhelp : ligne de commande/parent si disponible ;
- token du processus : SID/utilisateur ;
- `GetProcessTimes` : heure de création pour l’anti-réutilisation.

Référence : [IP Helper API](https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/).

### Cible sécurisée

Une carte externe conserve PID, creation time, executable et port. Juste avant l’arrêt, les quatre valeurs sont relues. Une différence produit `listenerChanged` et aucune action.

Windows n’offre pas un SIGTERM universel. L’UI et le CLI emploient le mot « Terminate » pour un processus externe. Aucun kill automatique, aucune escalade vers un autre PID. Pour Docker, le conteneur exact est toujours préféré.

### Cwd externe

Win32 ne fournit pas une API publique stable du cwd d’un autre processus. NetsuStack affiche : cwd connu pour ses propres runtimes ; dossier inféré de la ligne de commande avec libellé « inferred » ; sinon « unavailable ». Il ne lit pas le PEB distant.

## 6. Métriques

Le Job Object fournit la liste des PID actifs et des données d’accounting. Pour chaque PID :

- `GetProcessTimes` pour CPU ;
- `GetProcessMemoryInfo(PROCESS_MEMORY_COUNTERS_EX2)` ;
- `PrivateUsage` comme mémoire privée possédée ;
- `WorkingSetSize` comme RAM résidente.

Référence : [GetProcessMemoryInfo](https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getprocessmemoryinfo).

Formule CPU par processus entre deux échantillons :

```text
cpuPercent = delta(userTime + kernelTime) / deltaWallClock * 100
```

On ne divise pas par le nombre de cœurs : une charge de deux cœurs vaut 200 %, comme l’agrégation source. Les PID apparus entre deux échantillons commencent à 0 % pour éviter un pic artificiel.

Si un processus devient inaccessible entre énumération et lecture, il est ignoré. Une erreur partielle ne met pas à zéro tout le projet.

## 7. Santé et ports

- Résoudre `localhost` et essayer toutes les adresses ; un bind IPv6-only doit devenir sain.
- Utiliser un timeout par tentative et un budget global de 2 s.
- Ne pas considérer l’existence du PID comme preuve de santé si un port/URL est configuré.
- Avant start et après stop, interroger la table TCP, pas seulement tenter un bind local.
- Takeover attend 50 fois 200 ms maximum avant échec.

## 8. Docker Desktop

Découverte de `docker.exe` via PATH et emplacements Docker Desktop usuels. Algorithme :

```text
docker ps --filter publish=<port> --format {{.ID}}
docker inspect <id1> <id2>
sélectionner le binding HostPort exact
docker stop --time 10 <id>
```

Conserver ID, nom et labels Compose `project`/`service`. Ne jamais terminer `com.docker.backend.exe`, `vpnkit` ou un proxy global.

## 9. Logs et terminal

Deux buffers distincts par runtime :

- `RawTerminalBuffer` : bytes VT bornés à 2 MiB ou 20 000 lignes, utilisé pour replay xterm ;
- `PlainLogStore` : lignes nettoyées, `logBufferLines`, fichier rotatif.

Le parseur de logs doit gérer les séquences CSI, OSC terminées par BEL ou ESC-backslash, backspace, CR de progress bar, CRLF et chunks UTF-8 coupés au milieu d’un caractère.

## 10. Crash, santé et timeout

Le wait du processus racine ne suffit pas : la fin du Job Object ou la liste `ActiveProcesses==0` est la fin réelle du workload. Un shell peut sortir en laissant un enfant ; celui-ci reste possédé et doit déterminer l’état jusqu’à terminaison.

Pour un job temporaire :

- deadline basée sur une horloge monotone ;
- timeout marque `timedOut=true` avant l’arrêt ;
- exit code exposé `124` même si `TerminateJobObject` renvoie un autre code ;
- stop utilisateur expose `130`.

## 11. Limites `unsafe`

Chaque module Win32 :

- encapsule un type de handle distinct ;
- possède un constructeur sûr et `Drop` idempotent ;
- ne retourne pas de handle brut à `netsustack-supervisor` ;
- traduit `GetLastError` en `WindowsError { operation, code, message }` ;
- a un test d’intégration qui vérifie la fermeture des handles et l’absence d’orphelins.
