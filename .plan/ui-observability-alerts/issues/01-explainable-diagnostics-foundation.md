# 01 — Fondation des diagnostics explicables

**What to build:** Les calls et sessions normalisés produisent des diagnostics dérivés, lisibles et fiables : chaque alerte indique sa gravité, ses valeurs exactes ou inférées, les données indisponibles, son explication, son impact, sa recommandation et les sources à inspecter. Une `SignalPolicy` versionnée fixe centralement les seuils et les formules de tokens/cache, sans double comptage ni écriture dans les captures source.

**Blocked by:** Part 2 — 02 Signaux analytiques à l’échelle d’un call; Part 2 — 04 Diagnostic d’évolution du contexte par session.

**Status:** ready-for-agent

- [ ] Les diagnostics d’exécution, contexte, performance, cache et qualité de données sont dérivés côté Rust avec confiance, références source et explications actionnables.
- [ ] La politique de signaux et les DTO UI sont exposés en lecture seule; les totaux de tokens et taux de cache ne transforment jamais une donnée manquante en zéro.
- [ ] Des tests couvrent les seuils, gravités, indisponibilités, associations d’outils et le non-double-comptage.
