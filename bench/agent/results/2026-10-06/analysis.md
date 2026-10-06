# Analyse du pilote du 2026-10-06

Chiffres détaillés dans `report.md` (généré), données brutes dans `results.json`.

## Protocole

- 25 tâches en lecture seule : 18 sur semantiq @ `fa043c2`, 7 sur ripgrep 14.1.1 @ `4649aa97`.
  Répartition : 11 conceptuelles, 9 structurelles, 5 exactes (contrôle).
- 4 configurations × 3 répétitions = 300 runs, modèle `claude-sonnet-5-5`, Claude Code 2.1.291.
  - `baseline` (A) : aucun MCP.
  - `semantiq` (B) : uniquement le serveur MCP semantiq, même prompt système.
  - `semantiq-guided` : B + la consigne CLAUDE.md écrite par `semantiq init` (`--append-system-prompt`).
  - `cli-skill` (C) : aucun MCP, skill semantiq + CLI dans le PATH. **Aperçu uniquement** : binaire
    et SKILL.md pris sur l'état non commité de la branche `feat/cli-skill` (sha256 SKILL.md
    `d3bd92733075…`), donc à relancer quand la branche sera mergée.
- Coût total : 6,39 USD (équivalent API), soit environ 0,02 USD par run. Aucun run invalide.
- Re-scoring a posteriori (`--rebuild`) : la vérité terrain de deux tâches était incomplète.
  `sq-concept-path-guard` ne listait pas `semantiq-mcp/src/server.rs`, qui contient aussi des gardes
  `..`. `sq-concept-rename` n'acceptait que `process_events`, alors que `remove_file` désigne
  légitimement le mécanisme. Les deux corrections s'appliquent à toutes les configs.

## Résultats clés

| Catégorie | Config | Réussite | Tokens in | Coût moyen | Appels d'outils | dont semantiq |
|---|---|---:|---:|---:|---:|---:|
| conceptuel | baseline | 90 % | 21 955 | 0,020 | 1,8 | 0 |
| conceptuel | semantiq | 97 % | 25 761 | 0,020 | 1,9 | 0,0 |
| conceptuel | semantiq-guided | 97 % | 34 756 | 0,050 | 1,6 | 1,1 |
| conceptuel | cli-skill | 100 % | 36 114 | 0,028 | 2,2 | 0 |
| structurel | baseline | 100 % | 20 535 | 0,016 | 1,8 | 0 |
| structurel | semantiq | 100 % | 25 239 | 0,019 | 1,8 | 0,6 |
| structurel | semantiq-guided | 100 % | 25 821 | 0,019 | 1,6 | 0,9 |
| structurel | cli-skill | 100 % | 30 254 | 0,021 | 1,9 | 0 |
| exact | toutes | 100 % | 14 000 → 20 660 | 0,007 → 0,012 | 1,0–1,2 | ≤ 0,8 |

(Les valeurs exactes et les deltas sont dans `report.md` ; la ligne conceptuelle cli-skill reflète le
re-scoring.)

## Ce qu'on observe, honnêtement

1. **Pas de gain de réussite mesurable : le jeu de tâches sature.** Sur des dépôts de ~100 à 150
   fichiers Rust, Sonnet résout 96 % des questions avec grep et Read, en 1,7 appel d'outil en
   moyenne. Les +3 à +7 points de semantiq sur le conceptuel sont du bruit : `cli-skill`, qui n'a
   **jamais** appelé semantiq, obtient le même gain. Les échecs du baseline se concentrent sur une
   tâche (`sq-concept-calibration`, 1/3) qui passe 3/3 dans toutes les autres configs, y compris
   celle sans semantiq. Avec 3 répétitions, on ne distingue rien de mieux que ±10 points.

2. **Semantiq dégrade le coût, surtout quand on pousse à l'utiliser.**
   - Les définitions des 5 outils MCP coûtent **~1 575 tokens** de contexte fixe par requête
     (6 861 → 8 435 au premier tour). C'est +22 % de tokens d'entrée et +12 % de coût en moyenne,
     et +36 % de tokens / +25 % de coût sur les tâches exactes, où l'outil ne sert à rien. Ce coût
     est payé même quand semantiq n'est pas appelé (62 runs sur 75).
   - Avec la consigne de `semantiq init`, l'usage monte à 62/75 runs, mais le coût grimpe de
     **+90 %** (+155 % sur le conceptuel) sans gain de précision. Ce sont les résultats volumineux
     de `semantiq_search`, mis en cache à chaque tour, qui coûtent cher.
   - Le skill seul ne coûte que **~240 tokens** de contexte (sonde « Reply OK » : 9 767 → 10 006).
     Le surcoût apparent de `cli-skill` (+55 % de tokens) vient de l'activation du Skill tool et de
     la liste des skills intégrés de Claude Code (~3 000 tokens, artefact du harnais), pas du skill
     semantiq.

3. **L'adoption spontanée est faible et ciblée.** Sans consigne, l'agent appelle semantiq dans
   13 runs sur 75, uniquement sur des questions structurelles : `find_refs` 12 fois, `impact` 8,
   `deps` 1, et **jamais** `semantiq_search`. Sur le conceptuel, il lance toujours grep sur les
   mots de la question, et ça suffit. Le skill n'a été chargé dans **aucun** des 75 runs
   `cli-skill` : sa description ne déclenche pas pour ce style de question (le modèle répond en
   1 ou 2 Grep).

## Trois enseignements pour le produit

1. **Réduire le coût fixe et le volume des réponses avant tout.** Les descriptions et
   `outputSchema` MCP coûtent ~1,6 k tokens par tour, contre 240 pour le skill. Il faut une sortie
   de `semantiq_search` compacte par défaut (chemin, ligne et symbole, pas de snippets longs),
   des schémas allégés, et peut-être privilégier la voie skill/CLI. Sans cela, semantiq ne peut
   être rentable que sur des tâches longues.
2. **Le positionnement doit viser là où grep échoue, et le bench doit le montrer.** Sur des dépôts
   moyens, grep et un modèle fort suffisent. Le prochain jeu de tâches doit porter sur un gros dépôt
   (plusieurs milliers de fichiers, vocabulaire ambigu, homonymes fréquents comme `new`, `open` ou
   `parse`) et sur des questions multi-sauts plus profondes (impact à profondeur 3 et plus,
   « quels tests casser »). C'est là que `find_refs`, `impact` et la recherche sémantique peuvent
   faire la différence. Le harnais est prêt : il suffit d'ajouter un fichier `tasks/<repo>.json`.
3. **Travailler le déclenchement plutôt que la consigne générale.** Le « use semantiq first » de
   `semantiq init` force un usage coûteux et sans gain. À l'inverse, l'usage spontané de
   `find_refs`/`impact` sur les questions structurelles est pertinent. Mieux vaut des descriptions
   d'outils et de skill qui disent précisément *quand* semantiq bat grep (références réelles hors
   commentaires et chaînes, impact transitif, concept sans mot-clé) que de remplacer grep partout.

## Limites

- 3 répétitions seulement, et 4 runs en parallèle (bruit sur les durées, pas sur les tokens).
- Le prompt impose une liste `FINAL:`, ce qui favorise les réponses courtes et donc grep.
- Les durées moyennes (3 à 11 s) sont trop courtes pour mesurer un effet de latence.
- La config C a été mesurée sur un état WIP de `feat/cli-skill` ; à relancer avec
  `--configs cli-skill --resume --cli-bin … --skill-path …` après merge.
- L'indexation est lente pour la taille des dépôts : 63 s pour semantiq et 142 s pour ripgrep
  (~100 fichiers .rs, mais beaucoup de chunks de tests), à mettre en regard des runs de 5 s.
