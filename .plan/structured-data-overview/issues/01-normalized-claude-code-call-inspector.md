# 01 — Inspecteur de call normalisé Claude Code

**What to build:** Pour un call Claude Code capturé, l’utilisateur peut ouvrir une vue normalisée qui présente un seul fil : l’historique envoyé et la réponse reçue, avec rôles, blocs typés (`text`, `thinking`, `tool_use`, `tool_result`, image/autre) et tailles approximatives. La détection Claude Code est en place et un provider inconnu reste consultable en mode brut, sans casser l’UI.

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] Un call Claude Code se présente comme une conversation normalisée unique qui distingue l’historique envoyé du dernier tour reçu.
- [ ] Les blocs sont typés et leur taille approximative est visible ; les données brutes restent accessibles lorsqu’un provider est inconnu.
- [ ] La vue normalisée est exposée via une API read-only et affichable depuis l’UI.
- [ ] Le parsing est couvert par des tests sur des captures Claude Code représentatives.
