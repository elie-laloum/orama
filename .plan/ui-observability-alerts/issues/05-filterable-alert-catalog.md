# 05 — Inventaire d’alertes filtrable

**What to build:** L’utilisateur consulte l’inventaire complet des alertes dérivées, le filtre par gravité, catégorie, session ou call, puis ouvre un diagnostic uniforme et revient à la source exacte qui l’a produit. Les alertes restent recalculées et strictement en lecture seule.

**Blocked by:** 01 — Fondation des diagnostics explicables; 02 — Dashboard local temps réel; 03 — Explorateur de sessions et chronologie; 04 — Inspecteur d’appel à divulgation progressive.

**Status:** ready-for-agent

- [ ] L’inventaire est paginé, filtrable et présente gravité, constat court, date et source sans noyer les erreurs d’exécution.
- [ ] Chaque entrée ouvre une explication actionnable complète et des liens profonds vers session, call, bloc ou métrique.
- [ ] Aucun contrôle d’acquittement, de commentaire, de mutation ou de persistance d’alerte n’est proposé.
