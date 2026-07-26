# 04 — Diagnostic d’évolution du contexte par session

**What to build:** Dans une session, l’utilisateur visualise la croissance du contexte call après call, reçoit des alertes de compaction ou troncature lors de chutes significatives, et voit les changements du system prompt signalés comme du drift.

**Blocked by:** 03 — Navigation et timeline de sessions.

**Status:** ready-for-agent

- [ ] La vue de session affiche l’évolution du volume de contexte pour chaque call, avec les valeurs exactes disponibles et les tailles approximatives clairement étiquetées.
- [ ] Une chute significative du contexte ou de l’historique est signalée comme une compaction, une troncature ou une rupture à examiner.
- [ ] Un changement du system prompt entre calls est signalé comme du drift, avec assez de contexte pour identifier les calls concernés.
- [ ] Les signaux inter-call sont exposés via l’API read-only et validés par des tests de croissance, rupture et drift.
