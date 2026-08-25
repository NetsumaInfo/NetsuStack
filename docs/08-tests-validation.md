# Tests et validation

## 1. Principe

La parité est prouvée par le comportement, pas par la compilation. Chaque sous-système possède un test unitaire pur, un test d’intégration Windows si une primitive OS intervient, puis un scénario UI/CLI observable.

## 2. Tests Rust unitaires

### Domaine

- valeurs par défaut et décodage d’anciennes configs ;
- IDs `prj_`, `srv_`, `tmp_` ;
- résolution ID/nom/`project/server` ;
- mémoire 128 MiB–1 TiB, unités FR/EN et rejet des limites ;
- timeout 1 s–7 jours et arrondi ;
- actions uniques et non vides ;
- transitions d’état et backoff ;
- mapping codes jobs 0/124/130/non-zéro.

### Logs et diagnostics

- chunks UTF-8 divisés ;
- CSI, OSC, BEL, backspace, CR, CRLF ;
- ring buffer et rotation ;
- trois dépassements mémoire ;
- croissance par médianes, pic isolé ignoré ;
- seuils adaptés à 8/16/32/64 GiB ;
- détection doublons Next/Vite/Convex/npm/pnpm.

### API/CLI

- enveloppes et dates ISO-8601 ;
- routes/méthodes/codes ;
- token manquant/invalide/valide ;
- bind exclusivement loopback ;
- JSON stdout propre ;
- flags après argument positionnel ;
- aliases et erreurs d’ambiguïté.

## 3. Fixtures Windows

| Fixture | Comportement |
| --- | --- |
| `echo-server` | écoute IPv4/IPv6, endpoint santé configurable |
| `child-tree` | parent crée enfant/petit-enfant qui tient un port |
| `ansi-terminal` | couleurs, OSC, progress CR, demande input et resize |
| `crash-loop` | sorties programmables et période saine >30 s |
| `memory-growth` | allocation stable, pic, puis croissance soutenue |
| `port-reuse` | remplace rapidement un PID/port pour tester la revalidation |

Les fixtures sont de petits binaires Rust compilés dans le workspace, pas des scripts dépendant du shell.

## 4. Intégration Windows obligatoire

- ConPTY reçoit input et produit VT UTF-8.
- Resize modifie réellement colonnes/lignes vues par la fixture.
- Stop gracieux envoie Ctrl+C ; processus coopératif sort sans force.
- Processus non coopératif est terminé avec tout son Job Object après 5 s.
- Aucun descendant ne conserve le port après stop/restart/quit/update.
- Port scanner trouve IPv4-only, IPv6-only et double bind sans doublon.
- Takeover refuse un listener remplacé après snapshot.
- Docker test optionnel sur runner équipé ; parser Docker toujours testé par fixture JSON.
- CPU/mémoire de l’arbre augmentent avec la fixture et reviennent à zéro après arrêt.
- Config externe valide est appliquée après debounce ; JSON invalide conserve l’état précédent.
- Jobs terminés restent une heure selon horloge de test puis disparaissent.

## 5. Tests frontend

- store ignore une révision ancienne ;
- sélection se répare après suppression ;
- recherche couvre projet/dossier/serveur/port/localhost/commande ;
- chaque état serveur a texte, symbole et actions correctes ;
- terminal replay ne duplique pas les chunks ;
- formulaires valident conflits, timeout, mémoire, actions ;
- Resources conserve la dernière donnée sur erreur partielle ;
- dialogs destructifs exigent confirmation ;
- axe-core sans violation critique sur chaque destination.

## 6. E2E Tauri

Scénario nominal :

1. premier lancement et config vide ;
2. ajout projet fixture ;
3. détection et ajout serveur ;
4. start jusqu’à running/healthy ;
5. input terminal et ouverture localhost ;
6. restart, vérification nouveau PID et ancien arbre absent ;
7. action temporaire puis `wait` code 0 ;
8. job timeout puis code 124 ;
9. fermeture fenêtre, contrôle via tray/CLI ;
10. quit, confirmation que ports/processus sont libérés.

Scénarios secondaires : conflit/takeover, health unhealthy/recovery, limite mémoire, hot reload config, update handoff, agent setup idempotent.

## 7. Matrice manuelle

| Système | Shell | Installation | Obligatoire |
| --- | --- | --- | --- |
| Windows 10 22H2 x64 | cmd + Windows PowerShell | NSIS user | oui |
| Windows 11 x64 courant | PowerShell 7 + cmd | NSIS user | oui |
| Windows 11 x64 | Git Bash custom | NSIS user | smoke |
| Windows 11 x64 | Docker Desktop | NSIS user | oui pour release majeure |
| Windows 11 ARM64 | émulation x64 | NSIS x64 | informatif |

## 8. Performance

- app idle sans serveur : CPU moyen <0,5 % sur 60 s ;
- métriques 10 serveurs/100 processus : cycle <200 ms et pas de chevauchement ;
- événement snapshot ≤1 par intervalle métriques ;
- terminal soutient 5 MiB/min sans blocage UI ;
- mémoire transcript/log bornée selon limites ;
- sidebar 200 serveurs reste interactive.

Les seuils sont mesurés sur une VM 4 vCPU/8 GiB et une machine réelle Windows 11.

## 9. Critères de release

- tous les tests Rust/frontend/E2E verts ;
- aucune fuite/handle orphelin dans 100 cycles start-stop ;
- aucun port conservé après quit/uninstall ;
- app, CLI et installer Authenticode valides ;
- signature updater et `latest.json` vérifiés après téléchargement ;
- installation, update et uninstall testés depuis un compte non-admin ;
- docs CLI `--help`, config et skill correspondent au binaire ;
- audit manuel des capabilities/CSP/API token ;
- comparaison de la matrice Portly sans ligne non expliquée.
