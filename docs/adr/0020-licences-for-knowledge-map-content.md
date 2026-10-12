# ADR-0020: Licences for knowledge-map content

- **Status:** Accepted
- **Date:** 2026-10-12
- **Deciders:** Javier Hernandez

## Context

Knowledge maps ([ADR-0019](0019-knowledge-maps-on-the-answering-node.md))
distribute text from books and documentation, split into passages, with a
vector for each passage. [ADR-0018](0018-dependency-licences.md) limits
everything the project builds in or distributes to permissive licences, and
lets share-alike content (CC BY-SA) travel only as data with its
attribution.

Much of the best open reference material is under licences that ADR-0018
does not list: the GNU manuals and other documentation under the GFDL,
Wikibooks under CC BY-SA, documentation under the GPL. A map is not code:
it is not compiled into the binaries, it is downloaded only by the people
who choose it, and it is free. Nothing in a map is locked or encrypted.

Two things still matter. Splitting a book into passages and computing
vectors from them changes the work, so licences that forbid changes rule a
source out. Nodes earn credits for the answers they write, so licences that
forbid commercial use rule a source out too.

## Options considered

### Option 1: apply ADR-0018's list to map content as it stands

- Benefits: one rule for everything.
- Drawbacks: leaves out the GNU manuals, Wikibooks and most of the
  copyleft documentation that small models need most.

### Option 2: accept copyleft and share-alike content, as separate works

Each source keeps its own licence inside the map, the map adds no terms,
and maps stay separate from the code.

- Benefits: the largest body of open reference material becomes usable,
  without touching the licence of the code.
- Drawbacks: every source's terms must be honoured, and the attribution and
  licence texts must travel with each map.

### Option 3: accept any text that can be read for free

- Benefits: the widest coverage.
- Drawbacks: non-commercial and no-derivatives licences forbid exactly what
  maps do, and texts without a licence cannot be redistributed at all.

## Decision

Option 2. Map content may come from sources under these licences:

| Licence | Notes |
|---|---|
| Public domain | Including works of the US federal government |
| Any licence allowed by ADR-0018 | MIT, Apache-2.0, BSD, PSF-2.0 and the others on its list |
| CC BY, CC BY-SA (any version) | With attribution; share-alike content stays under its licence |
| GFDL | Without changes to invariant sections; notices kept |
| GPL and other copyleft licences, for documentation | The text stays under its licence |

Not allowed: non-commercial (NC) and no-derivatives (ND) licences, any
licence that forbids commercial use or changes, and text without a clear
licence.

### Key points

- **Maps are separate works.** They are downloaded on request and never
  built into the binaries or merged into the code. The project's licence
  (BSL 1.1, then Apache-2.0) does not apply to map content, and map content
  does not change the licence of the code.
- **Each source is a separate unit.** A source's passages and their vectors
  are distributed under that source's licence. The map is a collection of
  such units and adds no terms of its own.
- **Attribution travels with the map.** The manifest names each source's
  title, author, licence and origin, and states that the text was split
  into passages and that indexes and tables were dropped. The licence texts
  are in the package. The Maps panel shows sources and licences before a
  download.
- **Answers name their sources.** When an answer cites an excerpt, the node
  appends the title and licence of each source it cited.
- **Public domain is checked where maps are served.** A text in the public
  domain in one country may not be in another. Before a map is published,
  every source must be free to distribute where the project serves it.
- **ADR-0018 is unchanged for everything else.** Libraries, vendored code,
  models and datasets built into the project follow its list.

## Dependencies and licences

| Dependency | Version | Licence |
|---|---|---|
| None | | |

## Consequences

### Positive

- Maps can draw on GNU manuals, Wikibooks and other copyleft
  documentation.
- The licence of the code and the commercial licences offered under
  ADR-0017 are not affected.

### Tradeoffs

- Building a map takes care: each source needs its licence checked, its
  notices kept and its attribution recorded.
- Material under NC or ND licences stays out, however useful. Examples:
  *Pro Git* (CC BY-NC-SA) and *The Linux Command Line* (CC BY-NC-ND).
- To revisit if maps are ever sold, locked or built into the binaries.

## Implementation

### Where it lives

- The map manifest and the map builder (ADR-0019): licence, attribution and
  licence texts for every source.
- `CONTRIBUTING.md`: the licence rule for contributors, pointing here for
  map content.

### Next steps

1. Check the public-domain status of the first map's sources where maps
   are served (RFC-0001, open question 2).
2. Record the attribution and licence of each of the first map's 50 sources
   in its manifest.

## Related decisions

- ADR-0017: the licence of the code.
- ADR-0018: the licence list this decision adds to, for map content only.
- ADR-0019: what maps are and how they are distributed.

## References

- [RFC-0001](../rfcs/0001-knowledge-maps.md)
- Creative Commons licences, https://creativecommons.org/licenses/
- GNU Free Documentation License, https://www.gnu.org/licenses/fdl-1.3.html

## History

- v1.0 (2026-10-12): Initial decision.
