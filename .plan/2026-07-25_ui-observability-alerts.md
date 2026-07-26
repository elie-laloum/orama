# Partie 3 — UI d’observabilité et alertes explicables (issue du grilling)

Construire la vraie interface locale, modulaire et réutilisable du traceur après le
POC (partie 1) et la normalisation/sessions (partie 2). Elle transforme les données
normalisées et les signaux dérivés en un tableau de bord temps réel lisible : les
synthèses sont compactes, les détails sont accessibles par exploration progressive,
et chaque warning ou erreur explique clairement ce qui a été observé, pourquoi cela
compte et quelle source inspecter.

Cette partie **implémente** les détections et leurs explications ; elle ne construit
ni le shell Tauri ni une gestion mutable d’alertes.

---

## Décisions validées

| Sujet | Décision |
| --- | --- |
| Prérequis | Parties 1 et 2 terminées : capture SQLite, API brute, modèle normalisé, sessions et signaux de base |
| Cible immédiate | UI web locale servie par le processus Rust |
| Architecture cible | SPA web modulaire, découplée de l’API Rust et portable dans un futur shell Tauri |
| Frontend | **React + TypeScript + Rspack** |
| Visualisation | **Apache ECharts** pour séries, annotations, zoom, info-bulles et mini-graphiques |
| Mise à jour | **SSE** : nouveaux appels, agrégats et alertes sont poussés en temps réel |
| Porte d’entrée | Dashboard global compact, puis liste des sessions et détail session/appel |
| Densité | **Résumé puis exploration** : badges, chiffres courts et graphiques synthétiques fermés ; détails dans panneaux/accordéons |
| Tokens | Total = `input_tokens + output_tokens` ; lecture/écriture cache sont une ventilation de l’entrée, jamais additionnées une seconde fois |
| Cache | Synthèse = taux de réutilisation ; détail = volumes lus/écrits et état qualitatif |
| System prompt | Accordéon global, puis **un accordéon par bloc** avec résumé, taille et marqueur cache |
| Alertes | Catalogue complet, dérivé à la lecture, filtrable et strictement lecture seule |
| Explication | Diagnostic actionnable : gravité, constat, cause/hypothèse, impact, action conseillée et lien source |
| Seuils | Politique centralisée avec valeurs par défaut documentées et configurable ultérieurement sans changer les détecteurs |
| Données absentes | État neutre explicite (indisponible + raison), jamais zéro ou « sain » par défaut |

---

## Principes UX non négociables

1. **Pas de mur de texte.** Une ligne fermée ne contient que l’identité utile, les
   compteurs essentiels, les badges de gravité et un aperçu très court.
2. **Divulgation progressive.** Les textes complets, entrées/sorties de tools,
   ventilations de tokens, règles de calcul et JSON brut ne sont rendus qu’après une
   action explicite de l’utilisateur.
3. **Le signal précède les données.** Une anomalie résume d’abord le problème ; son
   panneau fournit ensuite les valeurs, son raisonnement, la recommandation et le lien
   ancré vers l’appel ou le bloc concerné.
4. **Les données brutes restent vérifiables.** Le panneau `Raw` est une vue secondaire,
   séparée, avec la capture source de vérité, pas la vue par défaut.
5. **Couleur + texte + icône.** Les gravités ne reposent jamais uniquement sur la couleur.
6. **Aucune fausse précision.** Les tokens API sont étiquetés `exact`; les tailles de
   bloc (`chars`, `bytes`, estimation `chars / 4`) sont explicitement `approx.`.

---

## Architecture cible

```
crate Rust (proxy + SQLite + parse + analytics + API)
 ├─ lecture des captures source de vérité
 ├─ normalisation et assemblage de sessions (partie 2)
 ├─ SignalPolicy + détecteurs + diagnostics explicables
 ├─ REST JSON : snapshots / drill-down / configuration en lecture seule
 └─ SSE : invalidations et événements de domaine
                                      │
                                      ▼
apps/web/ (React + TypeScript + Rspack)
 ├─ api/            client REST, contrat typé et client EventSource
 ├─ domain/         types partagés, formatters et mapping de présentation
 ├─ state/          cache de requêtes, agrégats et réconciliation SSE
 ├─ components/     primitives accessibles et composants réutilisables
 ├─ features/       dashboard, sessions, calls, alerts, raw
 ├─ charts/         adaptateurs ECharts isolés du domaine
 ├─ styles/         tokens de design, thèmes et styles globaux
 └─ app/            routes, composition, erreurs et états de chargement
```

- Le backend reste le propriétaire des définitions métier : calculs de tokens, cache,
  seuils, gravités et explications. Le frontend ne re-déduit pas les alertes à partir de
  valeurs affichées.
- Les DTO JSON stables sont isolés dans `api/`; les composants ne dépendent pas des
  réponses brutes SQLite ni du dialecte Anthropic.
- Les primitives React, les design tokens et les adaptateurs de données ne dépendent ni
  du serveur HTTP local ni de Tauri. Un futur Tauri charge le même bundle et remplace au
  besoin uniquement l’adaptateur de transport/bootstrapping.
- Rspack produit les assets versionnés. En production, Rust les sert depuis le bundle ; en
  développement, le dev-server Rspack proxifie REST/SSE vers Rust.

### Routage prévu

```
/                         Dashboard
/sessions                 Sessions, recherche et filtres
/sessions/:sessionKey     Timeline, analyses et appels de la session
/calls/:callId            Détail analytique d’un appel
/calls/:callId/raw        Capture JSON brute, vue secondaire
/alerts                   Inventaire filtrable de toutes les alertes actives
```

Les routes restent navigables par URL et les liens provenant d’un diagnostic conservent
l’ancre vers l’appel, le bloc, la métrique ou le segment concerné.

---

## Design system et composants

### Primitives communes

- `AppShell`, barre latérale compacte, fil d’Ariane et zone de contenu responsive.
- `MetricCard` : valeur principale courte, variation, état `available | unavailable`,
  info-bulle de définition et lien facultatif de drill-down.
- `SeverityBadge` : `critical`, `error`, `warning`, `info`, avec icône et libellé.
- `CacheBadge` : taux de réutilisation, état `high | low | cold | unavailable` et
  information textuelle accessible.
- `TokenSummary` : total, ventilation repliable et étiquette de fiabilité.
- `Disclosure` / `Accordion` accessible : bouton sémantique, clavier, `aria-expanded`,
  focus visible, ouverture individuelle ou groupée contrôlée.
- `DataState` : chargement, vide, indisponible, erreur de requête et valeur partielle ;
  chaque état indique l’action ou la raison utile.
- `SourceLink` : deep-link vers capture, appel, outil, bloc ou segment du system prompt.
- `DiagnosticPanel` : format uniforme d’une alerte, décrit ci-dessous.

### Dashboard

Le premier écran est un **résumé compact de santé récente** :

- cartes KPI : sessions actives/récentes, appels, tokens totaux, ratio cache, erreurs,
  warnings, TTFT et latence ;
- mini-graphiques de tendance avec période sélectionnable ;
- graphiques ECharts interactifs complets disponibles à l’ouverture des cartes ou dans
  les sections analytiques : volume de contexte/tokens, cache, TTFT/latence, répartition
  des gravités ;
- flux SSE des nouveaux appels/alertes, avec indicateur de connexion et reconnexion ;
- liste courte de sessions récentes : identifiant raccourci, modèle, durée, total tokens,
  badge cache et compteurs de gravité ;
- liste courte des alertes les plus importantes avec un lien de drill-down.

### Vue session

- en-tête compact : identifiant, modèle(s), période, nombre d’appels, total tokens et
  badges d’alertes ;
- timeline chronologique repliable par appel : horodatage, statut, TTFT, latence, total
  tokens, cache et gravités ;
- graphiques explorables : croissance de contexte, total entrée/sortie, ratio et volumes
  cache, TTFT/latence, événements de compaction et dérive du system prompt ;
- les anomalies sont des annotations cliquables sur les séries ; elles ouvrent leur
  `DiagnosticPanel` et ancrent la donnée source ;
- filtres : intervalle, gravité, catégorie, modèle, appels ayant des tools/erreurs/cache
  indisponible ;
- aucune session ne prétend être saine si des métriques requises sont indisponibles.

### Vue appel

L’en-tête n’affiche que le statut, l’heure, le modèle, le total tokens, le badge cache,
TTFT/latence et les alertes. Tout le reste est sectionné :

1. **Alertes** — compteur par gravité ; une ligne par signal fermée sur titre, mesure et
   source. L’ouverture révèle le diagnostic actionnable.
2. **Tokens et cache** — `input + output` comme total, puis entrée, sortie, cache lu et
   cache écrit. Le cache est exprimé en taux de réutilisation et volumes absolus. Les
   formules, le dénominateur et la fiabilité sont explicités.
3. **System prompt** — résumé (nombre de segments, taille approximative, segments cache),
   puis un accordéon pour chacun : indice, type/libellé si connu, aperçu court, taille
   approx., badge cache. L’ouverture révèle seulement le texte intégral et ses métadonnées.
4. **Conversation** — accordéon par tour, puis par bloc typé (texte, thinking, tool use,
   tool result, image, autre). La version fermée donne rôle, type, taille, éventuelle erreur
   tool et aperçu ; l’ouverture montre le contenu.
5. **Tools** — inventaire déclaré vs réellement appelé, associations `tool_use ↔ tool_result`,
   arguments et résultat repliés ; tout échec devient un signal lié.
6. **Raw** — lien vers la capture JSON brute, secondaire et explicitement étiquetée
   source de vérité.

---

## Tokens et cache : sémantique affichée

### Totaux exacts

Pour un appel lorsque `usage` est présent :

```
total_tokens = input_tokens + output_tokens
```

`cache_read_input_tokens` et `cache_creation_input_tokens` décrivent des portions ou
mécanismes liés à l’entrée selon l’API. Ils sont **affichés sous l’entrée** et ne sont
jamais réadditionnés au total. Les agrégats session/dashboard additionnent la même
formule par appel, avec une indication si des appels n’ont pas de `usage` exploitable.

### Taux de réutilisation cache

La carte cache affiche un taux défini et documenté par le backend :

```
cache_reuse_rate = cache_read_input_tokens / (input_tokens + cache_read_input_tokens + cache_creation_input_tokens)
```

Il est `unavailable` si le dénominateur ou les compteurs nécessaires manquent ; il ne
est pas transformé en `0 %`. Le détail affiche lectures/écritures et précise que le taux
mesure la part de l’entrée servie depuis le cache, non une économie monétaire universelle.

Les blocs et segments utilisent aussi un marqueur `cache_control` lorsqu’il existe dans
la requête : il montre un point de cache configuré, distinct d’une preuve de lecture
cache effective.

---

## Détections et diagnostics

### Modèle commun

Les détecteurs opèrent sur le modèle normalisé et les sessions, au moment de lecture ou
à l’émission d’un événement SSE. Ils produisent des alertes sérialisables :

```
Alert {
  id: stable derived id,
  category: execution | context | performance | cache | data_quality,
  severity: critical | error | warning | info,
  rule_id,
  title,
  summary,                     // une phrase compacte, fermée par défaut
  observed: [{ label, value, unit, exactness }],
  explanation,                 // cause constatée ou hypothèse explicitement qualifiée
  impact,
  recommendation,
  sources: [{ session_key, call_id, block_ref?, metric_ref? }],
  occurred_at,
  policy_version,
  confidence: exact | inferred | unavailable
}
```

`DiagnosticPanel` rend exactement : **gravité et règle**, **constat chiffré**,
**pourquoi/cause ou hypothèse**, **impact**, **conseil**, **source(s)**. Il ne présente
jamais une heuristique comme une certitude. Les diagnostics sont recalculés : pas
d’acquittement, commentaire, mutation ou persistance dans cette phase.

### Catalogue livré

| Catégorie | Détecteurs |
| --- | --- |
| Exécution | statut HTTP non réussi, erreur de proxy/transport persistée, réponse incomplète/stream interrompu, erreur de tool (`is_error`), tool use sans résultat associé et résultat sans appel associé |
| Contexte | compaction/troncature présumée (chute nette de messages et/ou d’input), dérive du system prompt (hash/diff), croissance anormale de contexte et croissance excessive d’un bloc ou résultat tool |
| Performance | TTFT trop élevé, latence totale trop élevée, régression relative dans une session lorsque suffisamment de points sont disponibles |
| Cache | taux de réutilisation faible ou nul lorsque l’entrée est assez grande, écriture cache sans réutilisation ultérieure observable, cache indisponible lorsque l’analyse cache est demandée |
| Qualité de données | `usage`, timestamps, corps reconstruit, association tool ou session key absents/incohérents ; mesure impossible ou approximation obligatoire |

Les règles de qualité servent à empêcher de fausses conclusions ; leur affichage ne doit
pas noyer les erreurs d’exécution. Les filtres séparent donc catégorie et gravité.

### Politique de seuils

Ajouter au core un `SignalPolicy` versionné, central et testé. Des valeurs par défaut
sont documentées et retournées par l’API, sans exposer de mutation dans cette phase :

```
context_growth:         ratio > 1.40 ET hausse >= 20_000 tokens
compaction:             chute relative input/messages documentée par politique
slow_ttft:              > 3 s
slow_latency:           seuil absolu documenté par politique
low_cache_reuse:        < 20 %, seulement si input >= minimum significatif
large_block:            seuil chars/bytes documenté par politique
tool_pairing:           toute paire manquante
```

La politique différencie les valeurs exactes API des approximations de taille. Elle est
injectée dans les détecteurs, pas dupliquée dans l’UI, afin que la configuration future
(par fichier, CLI ou préférences Tauri) n’exige pas de réécriture.

---

## API REST et SSE

Les endpoints bruts et normalisés de la partie 2 restent compatibles. Ajouter des DTO
versionnés et orientés UI :

```
GET /api/ui/dashboard?from=&to=          DashboardSnapshot
GET /api/ui/sessions?cursor=&filters=    SessionListPage
GET /api/ui/sessions/:key                SessionDetail + Alert[]
GET /api/ui/calls/:id                    CallDetail + Alert[]
GET /api/ui/alerts?filters=&cursor=      AlertListPage
GET /api/ui/signal-policy                SignalPolicy (lecture seule)
GET /api/ui/events                       text/event-stream
```

Événements SSE minimaux :

```
call.created        { call_id, session_key, timestamp }
call.completed      { call_id, session_key, changed_metrics }
session.changed     { session_key }
alerts.changed      { session_key?, call_id?, alert_ids }
dashboard.changed   { from? }
```

Le client traite ces événements comme des invalidations ciblées, récupère ensuite les
snapshots REST cohérents et déduplique/rejoue via `Last-Event-ID`. Il montre un état de
connexion, applique une reconnexion exponentielle et conserve un bouton d’actualisation
manuel. Le SSE ne remplace jamais la capture proxy ni le streaming relayé vers Claude.

---

## Plan d’implémentation

1. **Contrats et politique métier.** Ajouter `SignalPolicy`, les types `Alert` /
   `Diagnostic` / références source et les formatters de métriques ; fixer les formules de
   total et cache avec tests de non-double-comptage.
2. **Détecteurs explicables.** Étendre les signaux de la partie 2 avec le catalogue complet,
   les niveaux de confiance, les valeurs indisponibles et les liens source ; conserver les
   calculs côté Rust, dérivés sans modifier les captures.
3. **Agrégats et API.** Produire les snapshots dashboard/session/appel/alertes, pagination et
   filtres, puis le flux SSE d’invalidation ; garder tous les endpoints raw existants.
4. **Socle frontend.** Créer `apps/web` avec React, TypeScript, Rspack, client API typé,
   gestion SSE, routes, tokens de design, primitives accessibles et intégration du bundle
   statique côté Rust.
5. **Vues analytiques.** Réaliser dashboard, sessions et détail d’appel, avec ECharts isolé
   dans des adaptateurs, gestion complète des états vide/chargement/indisponible/erreur.
6. **Exploration progressive.** Réaliser les accordéons du system prompt par segment, de la
   conversation, des tools, des ventilations tokens/cache, des alertes et de la vue raw.
7. **Validation et finition.** Tests, données de démonstration issues de captures réelles,
   navigation source, accessibilité clavier, responsive local et vérification du flux SSE.

---

## Tests et critères de qualité

- **Rust unitaire :** chaque détecteur, gravité, seuil, explication, référence source,
  état indisponible et agrégat de tokens/cache.
- **Rust intégration :** des fixtures SQLite/captures Claude Code exercent REST + SSE,
  création d’alertes, compaction, dérive, erreurs tool et réponses sans `usage`.
- **Frontend unitaire :** formatters, mapping de DTO, calculs exclusivement d’affichage,
  `DataState`, rendu de `DiagnosticPanel`, accordéons et libellés cache/tokens.
- **Frontend intégration/E2E :** snapshot dashboard → drill-down session → appel → bloc /
  raw ; mise à jour SSE ; filtre alertes ; deep-link ; reconnexion SSE.
- **Accessibilité :** navigation clavier des disclosures, libellés de graphiques et badges,
  contraste, focus, statut live annoncé sans interruption excessive.
- **Contrats :** tests de compatibilité de schéma entre Rust et TypeScript ; aucun composant
  ne lit directement la forme brute Anthropic.

---

## Done-line explicite

La partie 3 est terminée lorsque :

- une UI **React + TypeScript + Rspack** locale, servie par Rust, offre dashboard, sessions,
  appels et alertes en temps réel par SSE, avec une architecture de composants portable
  vers Tauri ;
- les écrans fermés restent compacts (badges, chiffres, mini-tendances) et les détails
  riches sont uniquement accessibles par exploration/accordéons ;
- chaque appel affiche un total tokens non ambigu, sa ventilation entrée/sortie/cache, son
  état cache visuel et les raisons de toute donnée indisponible ;
- le system prompt est découpé en accordéons de blocs avec aperçu, taille et marqueur cache,
  sans injecter d’emblée le texte complet dans l’interface ;
- les erreurs, warnings de contexte, cache, performance et qualité sont réellement détectés
  côté Rust, configurés par une `SignalPolicy`, filtrables et expliqués avec valeurs,
  impact, recommandation et lien source ;
- les graphiques Apache ECharts permettent d’explorer tokens/contexte, cache, TTFT/latence
  et alertes sans remplacer les tableaux ni les sources vérifiables ;
- aucune alerte ou analyse ne transforme une donnée manquante en zéro ou en état sain, et
  aucune écriture additionnelle ne modifie la capture source de vérité.

---

## Hors périmètre explicite

- Implémentation du shell, packaging ou APIs natives **Tauri** (l’UI est seulement prête à
  être embarquée).
- Acquittement, masquage, commentaire, mutation ou persistance d’alertes.
- Configuration utilisateur des seuils dans l’UI ; seule la politique centralisée,
  documentée et lisible est livrée pour rendre cette extension future directe.
- Estimateur de prix/facturation multi-modèle : les tokens et ratios sont exposés, mais aucun
  coût monétaire n’est inféré sans une grille fiable.
- Nouveaux parsers de fournisseurs (OpenAI/Codex) au-delà du fallback normalisé existant.
