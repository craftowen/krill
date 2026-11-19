# Anti-AI Prose & Anti-Slop Visual Guidelines

Unguided LLMs regress toward high-probability phrasing and layout templates
(distributional convergence). Experienced readers spot these patterns immediately.
Apply the rules below during drafting and as a final edit pass before publishing.

---

## 1. Eliminating AI-Sounding Prose (English & Korean)

Empirical corpus studies (Kobak et al. 2025 on 15M PubMed abstracts; Juzek & Ward
COLING 2025 on RLHF lexical overrepresentation; Reinhart et al. PNAS 2025;
WikiProject AI Cleanup) show that "AI tone" comes from specific structural habits
rather than good grammar.

### 1.1 Structural & Rhetorical Bans

| Habit to Cut | Typical AI Tell (Avoid) | Human Editorial Replacement (Prefer) |
|---|---|---|
| **Trailing `-ing` / `~하며` significance inflation** | `"...reducing latency by 40%, underscoring the architecture's pivotal role in modern workflows."` / `"지연시간을 40% 줄였으며, 이는 차세대 환경에서 시사하는 바가 큽니다."` | Stop at the fact and comparison: `"zstd compression cut p99 latency by 40% (from 48 ms to 29 ms)."` |
| **Copula & simple-verb avoidance** | `"serves as"`, `"stands as"`, `"boasts"`, `"features"` / `"중추적 역할을 수행한다"`, `"강점을 자랑한다"`, `"검토를 진행하였다"` | Use direct verbs and copulas: `"is"`, `"has"`, `"검토했다"`, `"강점이 있다"`. |
| **Negative parallelism** | `"It is not just a cache, it is a paradigm shift."` / `"단순히 속도를 높이는 것을 넘어 새로운 기준을 제시합니다."` | State what the thing actually does in one affirmative clause. |
| **Vague authority hedging** | `"Industry experts note..."`, `"Critics argue..."` without named citations | Cite the specific paper, benchmark, file, or measurement, or cut the claim. |
| **Mechanical 3-bullet symmetry** | Splitting every section into 3 equal bullets starting with `**Bold Noun**: ...` | Match list length to reality (2, 4, 7 items). Mix full paragraphs with tables and figures so sentence length varies naturally (burstiness). |
| **Throat-clearing & recap sandwiches** | `"In this document, we will explore..."` / `"결론적으로 요약하자면 ~라고 할 수 있습니다."` | Open directly with the conclusion (BLUF). End when the last concrete point or action item finishes. |
| **Overworked RLHF buzzwords** | `delve`, `intricate`, `tapestry`, `pivotal`, `realm`, `landscape`, `testament`, `foster`, `seamless`, `혁신적인`, `패러다임`, `극대화` | Replace with domain-specific nouns and measured quantities. |

### 1.2 Korean Editorial Hygiene

- **Strip translationese prepositions**:
  - `~을 통해(서)` (*through*) -> rewrite with a concrete verb or instrument (`캐시를 붙여 응답 시간을 줄였다`).
  - `~에 대한 / ~에 있어서` (*about / in*) -> attach Korean case particles directly (`비용을 줄일 방법을 검토했다`).
  - `~에 의해` (*by*) -> flip to active subject (`데몬이 세션을 복구한다`).
- **Fix double passives & nominalizations**:
  - `해결되어진다` -> `해결된다` or `해결한다`; `보여진다` -> `보인다`.
  - `데이터의 분석을 실시함` -> `데이터를 분석함`.
  - Note: active voice and concise sentences are **defaults, not a 100% ban**. When
    the actor is unknown or the object is the natural topic, normal Korean passive
    forms and longer multi-clause sentences keep prose rhythm human.
- **Clean punctuation**:
  - Do not join metadata or prose clauses with decorative middle dots, spaced
    em-dashes, or emoji bullet prefixes. Use commas, periods, parentheses, or table
    columns instead.

---

## 2. Eliminating Visual & UI AI Slop

Anthropic's `frontend-design` calibration and `Design2Code` evaluations identify
two generations of templated UI tells that make HTML pages look machine-generated:

### 2.1 First- & Second-Generation Visual Tells to Avoid

1. **Cliché palettes**:
   - Purple-to-blue gradients on white cards, neon cyan/acid-green on pure `#000`,
     or Claude's own warm cream (`#F4F1EA`) paired with terracotta (`#D97757`) and
     high-contrast display serif. Use KADS `Paper` / `Tokyo Night` or `Light` /
     `Deep Sea` tokens instead unless the user's brand specifies exact hexes.
2. **Template eyebrow & heading ticks**:
   - Tracked-out `ALL-CAPS` eyebrow labels above every `<h2>`, italicizing or
     coloring a single "magic word" inside a headline, `WORD — fragment` labels, and
     appending `→` to every link or button.
3. **Fake sequence numbers**:
   - Putting giant `01 / 02 / 03` badges on unordered feature cards. Number items
     only when execution order or rank actually matters.
4. **The SaaS card chopper**:
   - Slicing short prose into identical rounded boxes with `box-shadow: 0 4px 12px rgba(0,0,0,.08)`
     at uniform 3-column widths regardless of content hierarchy. Use whitespace,
     `1px solid var(--ka-border)` rules, tables, and prose columns first; reserve
     cards for genuine KPIs, interactive widgets, or distinct entities.
5. **Gratuitous motion**:
   - Putting `fade-and-slide-up` scroll reveals on every section or `transform: translateY(-4px)`
     hover jumps on non-clickable cards. Animate only in response to user actions
     (opening disclosures, switching tabs, scrubbing a chart) and respect
     `prefers-reduced-motion: reduce`.

---

## 3. Pre-Publish Quality Gate

Before running `krill artifacts share`, verify:
- [ ] Every `<h1>` and `<h2>` reads as an informative conclusion or clear section task.
- [ ] Zero placeholder text (`Lorem ipsum`, `홍길동`, `TBD`, fake unlabelled numbers).
- [ ] Sensitive names, credentials, internal tokens, or private contract figures
      redacted or anonymized (public links are readable by anyone with the URL).
- [ ] `krill artifacts lint "<file>" --strict --json` exits `0`.
