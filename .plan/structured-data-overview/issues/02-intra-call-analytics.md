# 02 — Signaux analytiques à l’échelle d’un call

**What to build:** Dans un call normalisé, l’utilisateur voit immédiatement la ventilation exacte des tokens/cache, les segments du system prompt et leurs marqueurs de cache, les blocs les plus volumineux, et les outils déclarés comparés aux appels réels — arguments compris, avec appariement `tool_use` ↔ `tool_result`.

**Blocked by:** 01 — Inspecteur de call normalisé Claude Code.

**Status:** ready-for-agent

- [ ] La vue d’un call affiche les totaux exacts input, output, cache read et cache write lorsqu’ils sont fournis par le provider.
- [ ] Les segments du system prompt, les marqueurs de cache et les tailles approximatives des blocs sont clairement distingués.
- [ ] Les outils déclarés et les appels effectifs sont comparables, avec arguments et résultats appariés lorsque disponibles.
- [ ] Les signaux restent dérivés à la lecture et sont couverts par des tests sur des captures représentatives.
