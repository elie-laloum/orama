# 03 — Navigation et timeline de sessions

**What to build:** L’UI devient session-centrée : l’utilisateur sélectionne une session Claude Code, visualise ses calls dans l’ordre, et ouvre le call normalisé correspondant. Le groupement utilise l’identifiant de session natif ; l’analyse de préfixe sert à repérer les ruptures ou branches intra-session. Chaque entrée de timeline affiche modèle, statut, TTFT et latence.

**Blocked by:** 01 — Inspecteur de call normalisé Claude Code.

**Status:** ready-for-agent

- [ ] Les calls Claude Code sont regroupés en sessions à partir de leur identifiant natif, avec un comportement sûr quand cet identifiant manque.
- [ ] L’utilisateur peut parcourir une liste de sessions puis une timeline chronologique de leurs calls.
- [ ] La timeline expose modèle, statut, TTFT, latence et les éventuelles ruptures de chaîne intra-session.
- [ ] Les sessions et leur détail sont disponibles via l’API read-only et couverts par des tests.
