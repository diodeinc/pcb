# pcb-diode-api

Diode API client library for PCB toolchain integration.

## Features

- **Authentication**: OAuth2 flow with token refresh
- **Component Search**: Search and download components from Diode's component library
- **Registry Search**: Hybrid search across a local parts registry with trigram, word, and semantic indices
- **BOM Matching**: Match bill of materials against distributor availability

## Registry Search System

The registry search uses a hybrid retrieval approach combining three indices:

### Indices

| Index | Type | What it searches | Best for |
|-------|------|------------------|----------|
| **Trigram** | FTS5 | Canonicalized MPN (alphanumeric, uppercase) | Exact/partial MPN lookups |
| **Word** | FTS5 | Tokenized descriptions with prefix matching | Keyword-based queries |
| **Semantic** | Vector (1024-dim) | Titan embeddings via AWS Bedrock | Natural language queries |

### Fusion Algorithm

Results from all three indices are merged using **Reciprocal Rank Fusion (RRF)**.

#### RRF Formula

For each document `d` appearing in any ranker's top-20 results:

```
score(d) = Σ w_i / (K + rank_i(d))
```

Where:
- `w_i` = weight for ranker `i` (currently all 1.0)
- `K` = smoothing constant (10)
- `rank_i(d)` = 1-based position of document in ranker `i`'s results

#### Example Calculation

Query: `"voltage regulator 3.3v"`

| Document | Trigram Rank | Word Rank | Semantic Rank | RRF Score |
|----------|--------------|-----------|---------------|-----------|
| LDO_A    | -            | 1         | 2             | 1/(10+1) + 1/(10+2) = 0.091 + 0.083 = **0.174** |
| LDO_B    | -            | 3         | 1             | 1/(10+3) + 1/(10+1) = 0.077 + 0.091 = **0.168** |
| LDO_C    | -            | 2         | 5             | 1/(10+2) + 1/(10+5) = 0.083 + 0.067 = **0.150** |
| REG_X    | -            | 8         | -             | 1/(10+8) = **0.056** |

Documents appearing in multiple rankers naturally score higher (consensus effect).

#### Why RRF?

- **Robust to heterogeneous scales**: FTS5 returns negative ranks, vector search returns distances—RRF only uses ordinal positions
- **No query-type heuristics**: Works for both MPN lookups and descriptive queries without classification
- **Graceful degradation**: Empty index results contribute zero, no normalization artifacts
- **Fast**: O(n) where n ≈ 60 candidates, sub-millisecond fusion

### Current Parameters

```rust
const PER_INDEX_LIMIT: usize = 20;  // Top 20 from each ranker
const MERGED_LIMIT: usize = 50;     // Top 50 after fusion
const K: f64 = 10.0;                // RRF smoothing constant
const W_TRIGRAM: f64 = 1.0;         // Equal weights
const W_WORD: f64 = 1.0;
const W_SEMANTIC: f64 = 1.0;
```

## Search Pipeline & Next Steps

### Current Pipeline

```
Query
  │
  ├──► Trigram FTS (top 20)  ──┐
  ├──► Word FTS (top 20)     ──┼──► RRF Fusion ──► Top 50 Results
  └──► Semantic (top 20)     ──┘
```

### Planned: LLM Reranking

The next improvement is to add LLM-based reranking after RRF fusion:

```
Query
  │
  ├──► Trigram FTS (top 20)  ──┐
  ├──► Word FTS (top 20)     ──┼──► RRF Fusion ──► Top 50 ──► LLM Reranker ──► Final Results
  └──► Semantic (top 20)     ──┘
```

**LLM Reranker responsibilities:**
- Reorder candidates based on query-document relevance
- Handle nuanced/ambiguous queries
- Catch semantic mismatches (e.g., similar MPN but wrong category)
- Boost exact matches when appropriate

**Expected latency**: 100-500ms depending on model and batch size

### Future: Position-Aware Blend

Once user interaction data is available (clicks, selections), a position-aware blend can incorporate:
- Reranked order from LLM
- Original RRF scores
- User behavior signals (click-through rate, selection rate)

This enables continuous learning from user feedback.

## Usage

```rust
use pcb_diode_api::{RegistryClient, ParsedQuery};

// Open registry (downloads if not present)
let client = RegistryClient::open()?;

// Search with automatic preprocessing
let results = client.search("STM32G431", 25)?;

// Or use individual indices
let parsed = ParsedQuery::parse("voltage regulator");
let trigram_hits = client.search_trigram_hits(&parsed, 20)?;
let word_hits = client.search_words_hits(&parsed, 20)?;
```

## Embedding Generation

Semantic search uses AWS Bedrock Titan embeddings:
- Model: `amazon.titan-embed-text-v2:0`
- Dimensions: 1024
- Credentials: Fetched from Diode API, cached at `~/.pcb/aws-credentials.toml`
- Embedding cache: SQLite at `~/.pcb/registry/embedding_cache.db`
