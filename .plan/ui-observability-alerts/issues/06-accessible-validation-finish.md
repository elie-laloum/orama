# 06 — Parcours de confiance et finition accessible

**What to build:** Le parcours local complet — dashboard, session, call, blocs ou raw, puis inventaire d’alertes — est fiable, responsive et accessible. L’utilisateur peut l’explorer au clavier, comprendre les états dynamiques et vérifier que les mises à jour SSE, les liens source et les données indisponibles se comportent correctement.

**Blocked by:** 02 — Dashboard local temps réel; 03 — Explorateur de sessions et chronologie; 04 — Inspecteur d’appel à divulgation progressive; 05 — Inventaire d’alertes filtrable.

**Status:** ready-for-agent

- [ ] Les parcours dashboard → session → call → source/raw et alertes → source sont couverts contre des captures représentatives.
- [ ] Les disclosures, badges, graphiques et états live sont utilisables au clavier, correctement libellés et conservent un focus visible.
- [ ] Les contrats Rust/TypeScript, le responsive local, les états de données et la reconnexion SSE sont vérifiés par les tests appropriés.
