# NetsuStack

NetsuStack est l’équivalent Windows de [Portly](https://github.com/Melvynx/portly), réécrit en Tauri 2, Rust et React/TypeScript. L’implémentation suit la documentation de référence conservée dans ce dépôt.

## Origine du fork

Ce dépôt est un fork GitHub public de `Melvynx/portly`. Son historique Git d’origine est conservé, le remote `upstream` suit le projet source, et [le README original de Portly](docs/upstream-portly-README.md) reste archivé avec la documentation d’analyse. NetsuStack est une adaptation Windows indépendante publiée dans le respect de la licence MIT de Portly.

## Périmètre retenu

- Parité fonctionnelle complète avec Portly : application, zone de notification, terminal PTY, supervision, CLI, API locale, jobs temporaires, actions, santé, ports, Docker, ressources, limites mémoire, installation agent, démarrage automatique et mises à jour.
- Cible initiale : Windows 10 22H2 et Windows 11, x86-64, installation par utilisateur.
- Nom produit et commandes : `NetsuStack` / `netsustack`.
- Port API par défaut : `7737`, configurable.
- Portly reste une référence MIT ; NetsuStack ne dépendra pas de Swift, AppKit, SwiftTerm ni du superviseur Go.

## Documents

1. [Audit du dépôt Portly](docs/00-audit-portly.md)
2. [Spécification fonctionnelle](docs/01-specification-fonctionnelle.md)
3. [Matrice d’équivalence macOS/Linux → Windows](docs/02-matrice-equivalence-windows.md)
4. [Architecture cible Tauri 2](docs/03-architecture-tauri2.md)
5. [Contrats de données, API et CLI](docs/04-contrats-donnees-api-cli.md)
6. [Runtime Windows : ConPTY, processus, ports et métriques](docs/05-runtime-windows.md)
7. [Spécification UI React/TypeScript](docs/06-ui-react.md)
8. [Sécurité, installation et mises à jour](docs/07-securite-distribution.md)
9. [Stratégie de tests et critères de validation](docs/08-tests-validation.md)
10. [Feuille de route d’implémentation](docs/09-roadmap-implementation.md)

## Référence source

L’analyse est figée sur `Melvynx/portly@ed0e1b7` du 22 août 2026. Toute évolution ultérieure de Portly doit être traitée comme une nouvelle entrée dans la matrice de parité avant d’être intégrée.
