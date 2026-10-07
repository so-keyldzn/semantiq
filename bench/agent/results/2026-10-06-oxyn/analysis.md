# Analyse du bench oxyn du 2026-10-06

Chiffres détaillés dans `report.md` (généré), données brutes dans `results.json`, mesures
d'indexation dans `index-probe.json`.

## En tête : l'indexation

| Mesure | Valeur |
|---|---:|
| Fichiers indexés | 1 012 (418 Rust, 558 TS/TSX, 36 JSON/TOML/YAML/autres) |
| Symboles / chunks / dépendances | 32 446 / 5 221 / 10 396 |
| **Index complet (ONNX)** | **893 s (14 min 53)**, 3 541 s CPU, 0 erreur, DB 96 Mo |
| Même index, embeddings stub | 26 s |
| Part des embeddings | **~97 %** du temps |

- Machine : Apple M1 Max, 10 cœurs. Binaire : `semantiq` 0.10.1 installé (wrapper npm `semantiq-mcp`
  qui lance le binaire release ONNX, CodeRankEmbed INT8, 8 threads). Pendant l'index complet, la
  machine portait une charge légère (exploration `rg`).
- Le parsing, l'extraction et SQLite ne coûtent presque rien (26 s pour 297 k lignes). Le goulot est
  l'embedding des 5 221 chunks : environ 0,17 s par chunk, soit ~11 M de caractères passés au modèle.
- Les chunks embeddés vont en partie à du contenu qu'on ne cherche jamais sémantiquement : 1 369
  chunks (26 %) viennent de fichiers de tests ou de stories, et 206 de deux schémas JSON générés
  (`crates/oxyn-desktop/gen/schemas/*.json`, 103 chunks chacun, en tête du classement).
- Le pilote mettait 63 s (semantiq) et 142 s (ripgrep). Sur un dépôt de taille réelle, on passe à un
  quart d'heure avant la première requête utile.
- L'index a été construit une fois (sonde), puis réutilisé (`--reuse-index`) et copié pour le checkout
  `cli-skill` (voir « Corrections du harnais »). C'est pourquoi `meta.json` affiche `seconds: null`.

## Protocole

- 21 tâches en lecture seule sur oxyn @ `f2fb2ab` : 7 conceptuelles, 10 structurelles, 4 exactes.
  oxyn est un client de bases de données Tauri : workspace Rust `crates/` et `drivers/`, front-end
  React/TS `apps/desktop`, ~1 000 fichiers source, ~297 k lignes.
- Les tâches structurelles visent les cas où grep devrait montrer ses limites :
  - homonymes : `Backend::run` parmi 12 `fn run`, `Session::cancel` parmi des dizaines de `cancel`,
    `Driver::connect` face à `Backend::connect`, `commands::connect` et `Executor::connect` ;
  - chaîne d'appel sur 3 niveaux ;
  - implémenteurs de production parmi des doubles de test (`CatalogProvider` : 3 réels sur
    8 `impl`) ;
  - fichiers à corriger et tests à relancer si le trait `Session` change (13 fichiers) ;
  - dépendances transitives entre crates ;
  - composant rendu face à de simples mentions en commentaire ;
  - code mort TS.
- Vérité terrain vérifiée à la main dans le code (rg, lecture des définitions). Pour le code mort,
  vérification exhaustive par script : chaque `export function` de `apps/desktop/src` est cherché
  dans tout le dépôt, `.storybook` compris.
- 4 configurations × 2 répétitions = 168 runs, `claude-sonnet-5-5`, Claude Code 2.1.292, 3 runs en
  parallèle, plafond de 2 USD par run (jamais atteint). Aucun run invalide ni en erreur.
- **Coût total : 6,53 USD** pour les 168 runs, plus ~0,32 USD pour la sonde d'estimation (1 tâche × 4
  configs), soit **6,85 USD** sur un budget de 25 USD.

## Résultats clés

Après correction de la vérité terrain (voir plus bas).

| Catégorie | Config | Réussite | Tokens in | Coût moyen (USD) | Appels d'outils | dont semantiq | Durée (s) |
|---|---|---:|---:|---:|---:|---:|---:|
| conceptuel | baseline | 100 % | 38 918 | 0,039 | 3,9 | 0 | 10 |
| conceptuel | semantiq | 100 % | 53 625 | 0,046 | 4,2 | 0,0 | 13 |
| conceptuel | semantiq-guided | 100 % | 50 469 | 0,057 | 3,1 | 0,4 | 10 |
| conceptuel | cli-skill | 100 % | 57 086 | 0,054 | 4,4 | 0 | 12 |
| structurel | baseline | 100 % | 39 824 | 0,039 | 3,2 | 0 | 15 |
| structurel | semantiq | 100 % | 42 777 | 0,041 | 3,0 | 0,5 | 9 |
| structurel | semantiq-guided | 95 % | 43 912 | 0,039 | 3,0 | 0,8 | 9 |
| structurel | cli-skill | 100 % | 53 401 | 0,047 | 3,1 | 0,1 | 12 |
| exact | baseline | 100 % | 16 511 | 0,012 | 1,2 | 0 | 4 |
| exact | semantiq | 100 % | 23 862 | 0,014 | 1,2 | 0 | 4 |
| exact | semantiq-guided | 100 % | 24 772 | 0,014 | 1,2 | 0 | 5 |
| exact | cli-skill | 100 % | 24 012 | 0,019 | 1,2 | 0 | 4 |
| **total** | baseline | 100 % | 35 082 | 0,034 | 3,0 | 0 | 11 |
| **total** | semantiq | 100 % | 42 790 (+22 %) | 0,037 (+10 %) | 3,0 | 0,2 | 9 |
| **total** | semantiq-guided | 98 % | 42 452 (+21 %) | 0,040 (+19 %) | 2,7 | 0,5 | 9 |
| **total** | cli-skill | 100 % | 49 032 (+40 %) | 0,044 (+28 %) | 3,2 | 0,0 | 11 |

Contexte initial (premier tour) : 6 898 tokens pour le baseline, 10 160 avec le MCP semantiq
(+3 262), 10 569 en guided, 10 222 en cli-skill.

### Outils semantiq utilisés

| Opération | semantiq (runs) | semantiq-guided (runs) | cli-skill (runs) | Tâches concernées |
|---|---:|---:|---:|---|
| `semantiq_calls` | 4 | 4 | – | backend-run-callers, driver-connect-chain, cancel-chain |
| `semantiq_hierarchy` | 2 | 6 | – | catalog-provider-impls, command-sink-impls, driver-connect-impact |
| `semantiq_dead_code` | 2 | 2 | – | dead-exports |
| `semantiq_search` | 0 | 3 | – | local-tier, untrusted |
| `semantiq_repo_map` | 0 | 2 | – | large-results |
| `semantiq_find_refs`, `impact`, `deps`, `explain` | 0 | 0 | – | – |
| skill chargé / `semantiq dead-code` (CLI) | – | – | 1 / 1 | dead-exports |

- Sans consigne, semantiq est appelé dans **8 runs sur 42**, tous structurels, avec les nouveaux
  outils de la 0.10 (`calls`, `hierarchy`, `dead_code`). Ces runs ne sont jamais en échec.
- Avec la consigne de `semantiq init`, l'usage passe à 17 runs sur 42, et `semantiq_search` /
  `repo_map` apparaissent sur le conceptuel. Les runs en question réussissent, mais sans rien gagner
  sur le baseline, qui réussit aussi.
- `find_refs` et `impact`, les outils les plus appelés dans le pilote, ne servent plus du tout : ils
  ont été remplacés par `calls` et `hierarchy`, plus directs pour ces questions.

### Le skill est-il chargé spontanément ?

**Une seule fois sur 42 runs `cli-skill`** (0 sur 75 dans le pilote). C'était sur `ox-struct-dead-exports`,
la tâche la plus longue : le modèle a chargé le skill après deux Grep, puis lancé
`semantiq dead-code --path-prefix apps/desktop/src --include-public`, puis vérifié avec Grep. La
nouvelle description déclenche donc sur « unused code », mais pas sur « who calls », « implements »
ou « what breaks », alors que la description les cite. Sur ces questions, le modèle obtient sa
réponse en 1 à 3 Grep et n'en éprouve pas le besoin.

## Ce qu'on observe, honnêtement

1. **Même sur un dépôt de 1 000 fichiers, grep + Read sature : 100 % de réussite pour le baseline.**
   Les questions à homonymes (`run`, `cancel`, `connect`) ne gênent pas Sonnet : il cherche
   `self\.run\(` dans un seul crate, vérifie le `impl Backend` englobant, et répond en 2 appels. Sur
   les implémenteurs de trait, `impl X for` filtré par chemin suffit. Le seul échec de tout le bench
   (1/168) est un run `semantiq-guided` sans appel semantiq, où le modèle a inventé un type
   `RunningRegistry`. La difficulté de grep sur un gros dépôt n'est **pas** la justesse ; elle se
   voit sur la durée et le volume lus.

2. **Le seul gain net est sur les questions exhaustives : code mort et chaîne d'appel.**
   - `ox-struct-dead-exports` :
     - baseline : 10 appels, 134 k tokens, 69 s et 0,115 USD par run ;
     - semantiq : 4 appels (`semantiq_dead_code` + 2 Grep de vérification), 73 k tokens, 10 s
       et 0,075 USD (**-35 % de coût, -85 % de durée**) ;
     - guided : 0,068 USD, 19 s.

     Mais semantiq rate `AskAiButton` dans les 4 runs qui l'utilisent (rappel 0,80, réussite grâce
     au seuil), alors que le baseline le trouve. Le baseline, de son côté, inclut à tort
     `runStoryPreloads` une fois sur deux.
   - `ox-struct-driver-connect-chain` : `semantiq_calls` divise la durée par deux (7 s contre 15 s),
     à coût égal (0,069 contre 0,066 USD).
   - Sur l'ensemble du structurel, semantiq est 40 % plus rapide (9 s contre 15 s) pour +4 % de coût.
     Les durées sont bruitées (3 runs en parallèle), mais l'écart sur dead-exports est massif.

3. **Le coût fixe a doublé depuis le pilote.** Avec 9 outils (contre 5), les définitions MCP pèsent
   **~3 260 tokens** par requête, contre ~1 575 dans le pilote. Sur les tâches exactes, où semantiq ne
   sert à rien, cela donne +45 % de tokens et +17 % de coût. Sur le conceptuel, semantiq coûte +16 %
   sans un seul appel (14 runs sur 14 sans semantiq), et le guided +44 %, à cause des résultats
   volumineux de `search` et `repo_map`, par exemple 100 k tokens sur `large-results`. Le
   `cli-skill` paie ~3 300 tokens de liste de skills (artefact du harnais déjà décrit dans le pilote)
   pour un seul usage.

4. **Défauts d'index trouvés en vérifiant la vérité terrain** (semantiq 0.10.1, TS/TSX) :
   - **faux négatif** : la définition `export function AskAiButton({ … })`
     (`features/assistant/assistant-panel.tsx:539`) est enregistrée comme une *référence* dans
     `refs`, donc le symbole passe pour utilisé. C'est la cause du rappel de 0,80 ;
   - **faux positifs de `dead-code`** : `mentionMatch` (utilisé en `triggerFn={mentionMatch}`) et
     `cancelRefresh` (`onCancelRefresh={cancelRefresh}`) apparaissent comme morts. Les identifiants
     passés en valeur d'attribut JSX ne sont pas comptés comme références ;
   - **faux positif** : `runStoryPreloads` est utilisé uniquement dans `apps/desktop/.storybook/`,
     répertoire caché que l'indexeur ne parcourt pas ;
   - côté Rust, les fonctions référencées seulement par des attributs serde
     (`deserialize_with = "opaque_key"`, `skip_serializing_if = "is_false"`, `default = …`) sont
     signalées mortes en confiance « high ».

## Comparaison avec le pilote

| | Pilote (semantiq + ripgrep, ~110 fichiers) | oxyn (~1 000 fichiers) |
|---|---|---|
| Réussite baseline | 96 % | 100 % |
| Appels d'outils moyens (baseline) | 1,7 | 3,0 |
| Tokens in moyens (baseline) | ~20 k | 35 k |
| Coût moyen par run (baseline) | ~0,016 | 0,034 |
| Usage spontané de semantiq (config B) | 13/75 runs, `find_refs`/`impact` | 8/42 runs, `calls`/`hierarchy`/`dead_code` |
| Coût fixe MCP | ~1 575 tokens | ~3 260 tokens |
| Surcoût semantiq vs baseline | +12 % | +10 % |
| Skill chargé spontanément | 0/75 | 1/42 |
| Temps d'indexation | 63 s / 142 s | 893 s |

Le gros dépôt double le travail de l'agent (appels, tokens, coût) mais pas son taux d'échec.
Semantiq ne change pas la réussite. Il raccourcit nettement les tâches exhaustives et coûte un peu
plus partout ailleurs.

## Corrections de la vérité terrain (a posteriori)

- `ox-concept-secrets` et `ox-concept-large-results` : `min_recall` 0,67 → **0,66**. Avec 3 items
  attendus, 2/3 = 0,667 < 0,67, si bien que le seuil exigeait 3/3 alors que l'intention (et la
  convention du pilote, qui écrit 0,66 pour « 2 sur 3 ») était 2/3. Erreur objective de ma part, appliquée
  à toutes les configs. Avant correction : conceptuel baseline 86 %, semantiq 93 %, guided 86 %,
  cli-skill 93 %, et 7 échecs au total au lieu de 1. Les écarts entre configs ne changent pas de
  sens (bruit à ±1 run).
- Aucune autre correction : le seul échec restant (`RunningRegistry::cancel`) nomme un type qui
  n'existe pas.

## Corrections du harnais

- `init_guidance()` cherchait le littéral `claude_md_content` dans `init.rs`. Depuis la 0.10, ce bloc
  est construit en code (`claude_md_block`). Il est désormais produit par le binaire testé
  (`semantiq init --no-skill --no-index` dans un répertoire jetable), ce qui le garde aligné sur la
  version. Ce bloc recommande aussi la CLI via Bash, qui n'est pas autorisée dans `semantiq-guided` :
  c'est le texte réel du produit pour un utilisateur MCP.
- Le checkout `cli-skill` reçoit une copie de l'index (API de sauvegarde SQLite) au lieu d'une
  seconde indexation de 15 min, quand `--cli-bin` est le même binaire. Les fichiers sont identiques,
  le skill est en Markdown (non indexé) et les chemins sont relatifs. Vérifié : une requête CLI dans
  ce checkout répond en 0,4 s, sans réindexation.
- `report.py` ne comptait pas les commandes CLI `calls`, `hierarchy`, `dead-code` et `map` (regex
  antérieure à la 0.10). Le seul usage CLI du bench était donc invisible.

## Trois enseignements pour le produit

1. **L'indexation est le premier problème, et la solution est connue : séparer structure et
   embeddings.** 97 % des 15 minutes vont aux embeddings, alors que les outils réellement utilisés
   (`calls`, `hierarchy`, `dead_code`, et même grep chez l'agent) n'en ont pas besoin. Les
   embeddings de `search` n'ont servi que dans 3 runs guidés sur 168, sans gain. À faire : un index
   structurel prêt en ~30 s, des embeddings en arrière-plan ou opt-in, et l'exclusion par défaut des
   tests, stories et fichiers générés (≥ 30 % des chunks ici).
2. **La valeur démontrable est l'exhaustivité, pas la justesse : il faut la vendre et la fiabiliser.**
   Sur les questions « tout ce qui… » (code mort, chaîne d'appel), semantiq divise la durée par 2 à 7
   et le coût jusqu'à -35 %. C'est le seul endroit où il bat grep. Il le fait à condition que l'index
   soit juste, or il rate une définition TSX et compte mal les attributs JSX, les dossiers cachés et
   les références serde. Ce sont des bugs bien circonscrits (`ReferenceExtractor` TS/TSX, walker,
   heuristique serde de `dead_code`), à corriger avant d'en faire un argument.
3. **Le coût fixe des 9 outils MCP mange le gain : réduire la surface exposée.** ~3 260 tokens par
   requête (le double du pilote) payés sur chaque tâche, alors que 3 outils (`calls`, `hierarchy`,
   `dead_code`) concentrent plus de 80 % des appels semantiq (100 % sans consigne). Les pistes : un serveur MCP réduit par défaut
   (ces 3 outils plus `find_refs`), des descriptions et `outputSchema` plus courts, et des sorties
   compactes pour `search` / `repo_map` (jusqu'à 100 k tokens par run en guided). Le skill reste plus
   léger, mais il n'est chargé que dans les cas les plus évidents (1/42) : sa description doit
   déclencher sur « who calls / implements / what breaks », pas seulement sur « unused ».

## Limites

- 2 répétitions seulement, 3 runs en parallèle (bruit sur les durées, pas sur les tokens).
- Le jeu de tâches sature encore (167/168). Des questions encore plus exhaustives (impact à
  profondeur 3+ sur plusieurs crates, « tous les appelants TS d'une commande Rust », code mort Rust)
  seraient nécessaires pour mesurer un écart de justesse.
- Le prompt impose une liste `FINAL:` courte et invite à répondre de façon concise, ce qui favorise
  grep.
- Fuite d'isolation constatée : quand un résultat d'outil est trop long, Claude Code l'écrit dans
  `~/.claude/projects/<slug du checkout>/…/tool-results/` malgré `--no-session-persistence`, et un run
  l'a relu depuis ce chemin. Deux répertoires ont été créés (76 Ko et 24 Ko). Ils n'ont pas été
  supprimés, puisqu'on ne touche pas à `~/.claude`, et sont à nettoyer manuellement si on le souhaite.
  Aucune configuration n'a été lue ni modifiée.
