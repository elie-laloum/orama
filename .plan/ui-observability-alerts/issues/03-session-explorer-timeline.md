# 03 — Explorateur de sessions et chronologie

**What to build:** L’utilisateur peut filtrer et ouvrir une session, suivre ses calls chronologiquement et explorer les tendances de contexte, tokens/cache, TTFT et latence. Les compactions et dérives sont annotées et ouvrent le diagnostic et sa source; les métriques manquantes restent explicitement indisponibles.

**Blocked by:** 01 — Fondation des diagnostics explicables; 02 — Dashboard local temps réel.

**Status:** ready-for-agent

- [ ] Une liste filtrable de sessions mène à une vue de timeline avec modèle, statut, compteurs essentiels et gravités.
- [ ] Les graphiques et annotations permettent d’explorer croissance de contexte, cache, performance, compaction et dérive sans masquer les valeurs source.
- [ ] Les filtres, deep-links et états de données sont cohérents avec les snapshots REST et les invalidations SSE.
