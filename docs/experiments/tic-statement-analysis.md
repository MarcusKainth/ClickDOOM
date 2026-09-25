# Where the tic statement's analysis goes

The first of the two tic statements took about 44 s to analyse on ClickHouse
26.8.2.7. A resident session pays that once, and so does every
`run_statement` a test issues. This record finds the analyser pass that
carries it, tries the settings that reach that pass, and measures the
statement shape the pass skips.

## Question

Which part of the analyser carries the first statement's analysis? Does a
setting take it away? What does the statement have to look like for the
analyser to skip that work, and what does that shape cost?

## Method

The statement is `tick::bench::stage1(db, None, [Input::demo(20)])`, the
first statement over one row, built at `main` `10fdb99` and issued over HTTP
with `tick::PARSE_SETTINGS` against a database that
`support::resident::run` walked to tic 19. It is 2,239,360 bytes. The
wrapped form, the same statement with each comparison that is an operand of
`AND` or `OR` inside `identity()`, is 2,258,887 bytes. A resident session is
`support::resident::run` over 3 tics, and its first statement is the one
read back.

Analysis time is `ProfileEvents['QueryAnalysisMicroseconds']` from
`system.query_log`, keyed by `query_id` for a single statement and by the
database name for a session.

The profile is one run of the unwrapped statement with
`query_profiler_cpu_time_period_ns = 10000000`,
`query_profiler_real_time_period_ns = 0` and
`allow_introspection_functions = 1`. Each sample in `system.trace_log` is
counted once for every frame its stack holds, so a share is the part of the
run spent inside that frame.

The operand count comes from `EXPLAIN QUERY TREE passes = 1` of the
statement's `SELECT`. A script walks that tree for every `and` and `or`
node and every operand the pass hashes. An operand's cost is the nodes in its
non-constant side plus the nodes in each subquery a column in it names as its
source, which is what one `getTreeHash` call on it visits.

The mechanism is read from the ClickHouse sources at tag `v26.8.2.7-lts`.

## Conditions

| | |
|---|---|
| Date | 2026-09-25 |
| ClickHouse | 26.8.2.7, image `clickhouse/clickhouse-server:26.8.2.7`, one container with `docker/clickhouse/config.d` and `users.d` mounted |
| Machine | Apple Silicon, 18 cores |
| Load | 1-minute load 1.5 to 7.8 across every timed run; a run above 8 at its start or end would be discarded, and none was |
| Lock | `scripts/machine-lock.sh` held for every timed run except the two 300-tic walks |

The machine was not quiet. A four-server ClickHouse cluster with three
keepers from another project ran throughout, outside the machine-lock
protocol. The before and after runs are interleaved in one window, so their
ratio holds. The absolute seconds carry that condition.

## Numbers

### The profile

The unwrapped statement, 41.34 s of analysis, load 3.75 at the start and 3.25
at the end:

| Frame on the stack | Samples | Share |
|---|---:|---:|
| every sample | 4,368 | 100% |
| `IQueryTreeNode::getTreeHash` | 3,826 | 87.6% |
| `LogicalExpressionOptimizerVisitor` | 3,819 | 87.4% |
| `tryOptimizeAndCompareNotEqualsChain` | 2,967 | 67.9% |
| `tryReplaceOrEqualsChainWithIn` | 832 | 19.0% |
| `tryOptimizeAndCompareChain` | 20 | 0.5% |
| `QueryAnalysisPass::run` | 16 | 0.4% |

Under `getTreeHash`, `IDataType::updateHash` holds 1,568 samples and
`ColumnNode::updateTreeHashImpl` 1,313. The wrapped statement, profiled the
same way, has 361 samples, and the 22 of them inside the visitor are all in
`tryOptimizeAndCompareChain`.

### The settings

Each row is three runs of the unwrapped statement, interleaved with the
other rows.

Load 1.5 to 6.1.

| Setting | Runs | Median |
|---|---|---:|
| none | 39.87, 39.61, 39.60 s | 39.61 s |
| `optimize_and_compare_chain = 0` | 39.16, 39.32, 39.83 s | 39.32 s |
| `optimize_min_equality_disjunction_chain_length = 1000000` | 39.64, 39.75, 39.18 s | 39.64 s |
| `optimize_min_inequality_conjunction_chain_length = 1000000` | 39.95, 39.93, 40.03 s | 39.95 s |
| `optimize_redundant_comparisons = 0` | 40.09, 39.59, 39.06 s | 39.59 s |
| `optimize_extract_common_expressions = 0` | 40.05, 39.39, 45.08 s | 40.05 s |
| the five above together | 38.94, 39.18, 42.38 s | 39.18 s |
| `compile_expressions = 0` | 39.89, 39.29, 43.87 s | 39.89 s |
| `query_plan_enable_optimizations = 0` | 40.04, 39.20, 46.05 s | 40.04 s |

No setting reaches `tryOptimizeAndCompareNotEqualsChain` or
`tryReplaceOrEqualsChainWithIn`. With `enable_analyzer = 0` the statement
does not run: `Code: 43. DB::Exception: First argument for function
tupleElement must be Tuple, Nullable(Tuple), QBit, JSON or array of these.
Actual Int64`.

### The mechanism

`LogicalExpressionOptimizerVisitor::enterImpl`
(`src/Analyzer/Passes/LogicalExpressionOptimizerPass.cpp`) calls
`tryReplaceOrEqualsChainWithIn` for every `or` node and
`tryOptimizeAndCompareNotEqualsChain` for every `and` node.
`optimize_and_compare_chain` gates only the `tryOptimizeAndCompareChain`
call beside them. `optimize_and_compare_chain_max_hash_work` bounds only
that one function too.

`tryOptimizeAndCompareNotEqualsChain` looks at each argument of the `and`.
When the argument is one of `equals`, `notEquals`, `less`, `lessOrEquals`,
`greater` or `greaterOrEquals` with a constant on one side,
`addComparisonFilter` stores it in `filter_map[expression]`, where
`expression` is the other side. That map is keyed by
`QueryTreeNodeWithHash`, whose constructor calls `getTreeHash` on the node
(`src/Analyzer/HashUtils.h`). The lookup happens with
`optimize_redundant_comparisons` off as well, and for an `and` with a single
such operand.
`tryReplaceOrEqualsChainWithIn` does the same for each `equals` with a
constant under an `or`, through `node_to_constants[expression]`.

`IQueryTreeNode::getTreeHash` (`src/Analyzer/IQueryTreeNode.cpp`) walks the
node's subtree. For a `ColumnNode` it also pushes the column's source as a
weak node, and hashes that node's whole subtree the first time the call meets
it. The hash is not cached, so the next call hashes it again. A column read
from a `FROM` subquery is a column whose source is that subquery, so hashing
`prev_x` for the operand `prev_x = 0` hashes every stage below the one it
sits in.

`identity()` (`src/Functions/identity.h`) returns its argument column, is
not suitable for constant folding, and has the argument's own type. Its
function name is not a comparison, so an `and` operand `identity(a = 1)` is
kept as it is and nothing is hashed. It is not suitable for short-circuit
execution on its own, but `findLazyExecutedNodes`
(`src/Interpreters/ExpressionActions.cpp`) makes a node lazy when any of its
children is, so a lazily executed argument of the comparison stays lazy
through it. `materialize()` on the constant side would also leave the pass
nothing to hash, but it changes which comparisons the analyser folds to
constants.

### The operands and where they sit

From the unwrapped statement's query tree: 186,407 nodes, 877 `and` nodes
and 339 `or` nodes. 1,676 operands are hashed, 49,505,295 nodes in one run
of the pass. 311 of the `and` nodes and 73 of the `or` nodes hold exactly one
hashed operand.

The ten bindings whose operands hash the most:

| Binding | Operands | Nodes hashed | Share |
|---|---:|---:|---:|
| `tk_folded` | 148 | 4,863,823 | 9.8% |
| `mt_missile_folded` | 148 | 4,189,187 | 8.5% |
| `cw` | 178 | 3,612,134 | 7.3% |
| `mt_thrown` | 30 | 2,208,114 | 4.5% |
| `mt_missile_draws` | 53 | 2,045,025 | 4.1% |
| `mt_missile_unsure` | 33 | 1,487,780 | 3.0% |
| `mt_hurt` | 65 | 1,427,409 | 2.9% |
| `tx_crossed_line` | 8 | 1,045,160 | 2.1% |
| `mv` | 116 | 968,510 | 2.0% |
| `tx_two` | 17 | 855,969 | 1.7% |

The largest single operands each hash about 186,000 nodes, the whole
statement: they compare a column of the outermost stages, whose source
subquery holds everything below. Operands sit at subquery depths 0 to 48,
and no single level holds most of the cost. The wrapped statement's tree has
4 hashed operands left, 216 nodes in all.

### The wrapped statement

Four interleaved rounds, first statement:

| Round | Load, lowest to highest | Bench, unwrapped | Bench, wrapped | Bench, wrapped, `optimize_and_compare_chain = 0` | Session, unwrapped | Session, wrapped |
|---|---|---:|---:|---:|---:|---:|
| 1 | 6.60 to 7.75 | 46.59 s | 1.33 s | 1.00 s | 44.14 s | 1.49 s |
| 2 | 5.77 to 6.84 | 44.61 s | 1.33 s | 1.05 s | 45.79 s | 1.50 s |
| 3 | 5.08 to 6.59 | 44.61 s | 1.32 s | 1.04 s | 44.11 s | 1.36 s |
| 4 | 3.11 to 5.08 | 42.06 s | 1.20 s | 0.98 s | 43.75 s | 1.24 s |
| median | | 44.61 s | 1.32 s | 1.02 s | 44.12 s | 1.42 s |

The session's second statement analyses in 1.05 s unwrapped and 0.37 s
wrapped, medians of the same four rounds.

Two 300-tic resident walks of demo3, one unwrapped and one wrapped, write the
same `native_state` and `native_stage` rows: 44,548 and 45,300
`(tic, column)` pairs compared by `cityHash64`, none differing. The same
comparison against a copy with one value changed finds that one pair. Per
tic, `processors_profile_log` gives the first statement 42.49 ms unwrapped
and 44.02 ms wrapped, one walk each at load 6 to 7.

### The minimal repro

A `SELECT` whose condition is an `AND` of K comparisons `c != n`, over D
nested subqueries that each pass `c` up (the script is in the draft below).
Three runs each at load 2.8 to 4.1, medians:

| D | K | Condition | Analysis |
|---:|---:|---|---:|
| 0 | 2,000 | `c != n` | 0.035 s |
| 25 | 1,000 | `c != n` | 0.243 s |
| 50 | 250 | `c != n` | 0.224 s |
| 50 | 500 | `c != n` | 0.327 s |
| 50 | 1,000 | `c != n` | 0.540 s |
| 50 | 2,000 | `c != n` | 0.969 s |
| 50 | 4,000 | `c != n` | 1.783 s |
| 50 | 2,000 | `identity(c != n)` | 0.142 s |
| 50 | 2,000 | `c NOT IN (n)` | 0.136 s |
| 50 | 2,000 | `c = n`, joined by `OR` | 1.677 s |

Analysis grows with K times D. `identity()` and `NOT IN` take it back to
what the subqueries alone cost.

## Verdict

About 87% of the first statement's analysis on 26.8.2.7 is
`LogicalExpressionOptimizerPass` hashing the non-constant side of each
comparison in an `AND` or `OR` chain, and each hash walks the stage
subqueries below it. No setting turns that off. Wrapping each such comparison
in `identity()` takes the first statement from 44.6 s to 1.3 s and leaves
every row the same. The generator does that in
`native/src/sql/sim/chains.rs`. `optimize_and_compare_chain = 0` takes another
0.3 s off the wrapped statement.

## Draft upstream issue

Not filed. The text below is ready to file against ClickHouse/ClickHouse as a
performance issue.

> Title: LogicalExpressionOptimizerPass rehashes the FROM subquery for every comparison in an AND or OR chain
>
> ### Describe the situation
>
> On 26.8.2.7, the analysis of a query whose condition is an `AND` of
> comparisons against constants grows with the number of comparisons times
> the depth of the subqueries under it. A generated query with 1,676 such
> comparisons over up to 48 levels of nested subqueries spends about 44 s in
> analysis, 87% of it under `LogicalExpressionOptimizerVisitor` and
> `IQueryTreeNode::getTreeHash`.
>
> `LogicalExpressionOptimizerVisitor::enterImpl` calls
> `tryOptimizeAndCompareNotEqualsChain` for every `and` node and
> `tryReplaceOrEqualsChainWithIn` for every `or` node, and no setting gates
> either. Both key a map by the non-constant side of each
> comparison-with-constant operand (`filter_map[expression]`,
> `node_to_constants[expression]`), and `QueryTreeNodeWithHash` calls
> `getTreeHash` on it. `getTreeHash` also hashes a column's source in full,
> so a column read from a `FROM` subquery hashes that whole subquery, and
> nothing caches it between calls. This happens for an `and` with a single
> such comparison too.
> `optimize_and_compare_chain` and `optimize_and_compare_chain_max_hash_work`
> reach only `tryOptimizeAndCompareChain`.
>
> ### How to reproduce
>
> ```bash
> # upstream.sh D K SHAPE: an AND (or OR) chain of K comparisons over D
> # nested subqueries. SHAPE: ne, identity, notin, eq_or.
> D=$1; K=$2; SHAPE=$3
> q="SELECT number AS c FROM numbers(1)"
> for i in $(seq "$D"); do
>     q="SELECT c + $i AS c, arrayMap(x -> x * c, range(10)) AS a FROM ($q)"
> done
> case $SHAPE in eq_or) cond="0" ;; *) cond="1" ;; esac
> for n in $(seq "$K"); do
>     case $SHAPE in
>         ne) cond="$cond AND c != $n" ;;
>         identity) cond="$cond AND identity(c != $n)" ;;
>         notin) cond="$cond AND c NOT IN ($n)" ;;
>         eq_or) cond="$cond OR c = $n" ;;
>     esac
> done
> echo "SELECT $cond FROM ($q) FORMAT Null"
> ```
>
> ```bash
> ./upstream.sh 50 2000 ne > q.sql
> curl -sS 'http://localhost:8123/?max_query_size=0' --data-binary @q.sql
> # then QueryAnalysisMicroseconds from system.query_log
> ```
>
> | D | K | Condition | Analysis, median of 3 |
> |---:|---:|---|---:|
> | 0 | 2,000 | `c != n` | 0.035 s |
> | 50 | 1,000 | `c != n` | 0.540 s |
> | 50 | 2,000 | `c != n` | 0.969 s |
> | 50 | 4,000 | `c != n` | 1.783 s |
> | 50 | 2,000 | `identity(c != n)` | 0.142 s |
> | 50 | 2,000 | `c NOT IN (n)` | 0.136 s |
> | 50 | 2,000 | `c = n`, joined by `OR` | 1.677 s |
>
> ### Expected performance
>
> Close to the `identity` and `NOT IN` rows. Hashing a column without its
> source's whole subtree, or caching tree hashes for the length of the pass,
> would remove most of it.
>
> ### Workaround
>
> Wrapping each comparison in `identity()` keeps the pass from matching it.
> Analysis of the generated query drops from 44.6 s to 1.3 s, with the same
> results.
