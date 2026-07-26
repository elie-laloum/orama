# 02 — Dashboard local temps réel

**What to build:** L’utilisateur ouvre une interface React locale, servie par Rust, sur un résumé compact de santé récente : sessions, calls, tokens, cache, erreurs, TTFT, latence et diagnostics prioritaires. Il peut ouvrir des tendances interactives et recevoir les changements par SSE, avec reconnexion, actualisation ciblée et bouton de rafraîchissement manuel.

**Blocked by:** 01 — Fondation des diagnostics explicables.

**Status:** ready-for-agent

- [ ] Le bundle React + TypeScript + Rspack est servi localement et sépare les contrats API, transport, état, primitives et présentation pour rester portable vers Tauri.
- [ ] Le dashboard affiche des KPI, sessions et alertes récentes, avec graphiques explorables et états explicites de chargement, vide, erreur ou indisponibilité.
- [ ] Les événements SSE invalident les données concernées et la reconnexion ne présente jamais des métriques obsolètes comme fiables.
