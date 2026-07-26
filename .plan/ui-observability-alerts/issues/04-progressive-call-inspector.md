# 04 — Inspecteur d’appel à divulgation progressive

**What to build:** Depuis le dashboard ou une session, l’utilisateur ouvre un call avec son statut, modèle, tokens/cache exacts, latence et alertes. Des disclosures accessibles révèlent ensuite les diagnostics, segments du system prompt, conversation typée, outils déclarés/appelés et résultats associés; la capture brute reste une source secondaire clairement identifiée.

**Blocked by:** 01 — Fondation des diagnostics explicables; 02 — Dashboard local temps réel.

**Status:** ready-for-agent

- [ ] La vue fermée reste compacte et chaque détail riche est ouvert seulement sur action explicite au clavier ou à la souris.
- [ ] Les ventilations de tokens/cache, les segments et blocs approximatifs, les outils et les diagnostics conservent exactitude, contexte et liens vers leurs sources.
- [ ] La vue Raw reste navigable mais distincte de la vue analytique et ne remplace pas les diagnostics normalisés.
