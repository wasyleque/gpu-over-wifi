<!-- i18n-switch -->
**English** · [Polski](DESIGN-bulk-channel.md) · [Español](DESIGN-bulk-channel.es.md)
<!-- /i18n-switch -->

# A separate channel for large memory writes (design, not implemented)

Status: **plan saved 2026-11-22, none of this is implemented** except for the instrumentation (query time histogram, point 0 below). Tests and the deployment decision after
measurements on the second game (Shadow of the Tomb Raider) and on Cyberpunk. Goal: smoothness (fewer frames > 60 ms during resource loading), not average FPS.

## 1. Problem

Everything the client sends to the server goes over a single TCP connection in a single FIFO queue (`Out` in `crates/gpu-icd/src/link.rs`, capacity `MAX_QUEUED_BYTES` = 64 MB,
option `GPU_ICD_SENDQ_MB`): command batches (`Cmds`), memory writes (`MemWrite*`, compressed by threads `gpu-icd-frameN`), and queries with responses (`Query`, `Sync`).
A query that enters the queue waits behind everything already in it, and on the server side a single thread executes requests in order.

Measured (Rise of the Tomb Raider, DX11/DXVK, Ryzen 5 1600X, 2.5 Gb/s, vsync off, 2026-11-21/22):

* on average, a query with a response waits in the queue 261 µs, and from write to response 798 µs, so the average queue cost is small (about 0.4 ms per frame);
* tail: dumps `GPU_ICD_SPIKES=60` show during the resource loading phase queries `vkGetImageMemoryRequirements2` 32-95 ms, `vkGetDescriptorEXT` 43-46 ms, `vkCreateImage`
  with a 69 ms wait for the instance lock, `vkGetSemaphoreCounterValue` 71 ms; in benchmark scenes 11-12 frames > 60 ms out of about 975 (scenes 3 and 2), frame p99 62-64 ms at p50 23-29 ms;
* at link saturation (run with LZ4, 230 MB/s) these times grew to 100 ms and frames stalled (`hamulec_klatek` 100 ms).

Expected gain: average FPS +1-3% (upper bound, because the average queue cost is about 1% of a frame), while **smoothness** (tail of the distribution) may improve markedly.

## 2. Idea

Large memory writes go over a separate connection (`AttachBulk{session}`, op to be allocated), and the control stream carries only commands and queries, so a query does not wait behind
megabytes of writes. Consistency is ensured by sequence numbers, as in the existing read channel (`AttachReads`, `MemReadAfter`, `ReadView`):

1. **Barrier before using writes.** The client numbers writes on the bulk channel (u64, without wraparound: the 16-bit frame number bug that surfaced on 2026-11-21 must not return).
   Before a command batch that contains `vkQueueSubmit*` (and before every memory read request), it inserts `BulkBarrier{upto}`; the server executes the barrier, waiting until the bulk thread
   applies all writes to `upto`. This is exactly today's ordering guarantee ("writes before submission") expressed by a number instead of a position in a single stream.
2. **Write after allocation.** A write on the bulk channel carries `after_ctrl` (the control frame number after which the memory exists and is mapped); the bulk thread waits for `ReadView::wait_for`
   (the same functions and asymmetric window as the read channel) before writing. Without this, a write could arrive before `vkAllocateMemory`/`vkMapMemory`.
3. **Memory release.** Before `vkFreeMemory`/`vkUnmapMemory` a barrier as in point 1 (writes to this memory must finish).
4. **Mirror (zstd prefix).** Writes within the bulk channel are ordered (one connection), so the client and server mirrors remain consistent; writes to THE SAME memory never
   go over two paths simultaneously (the bulk channel replaces the control stream for memory writes entirely, not only for large ones).
5. **The bulk thread on the server side** requires access to mappings and the mirror: today the mirror lives in `Replayer` (`&mut`, the service thread). It must be moved to a shared structure
   (`Arc`, per-memory lock) or given to the bulk thread exclusively (then differential reads, which currently use the mirror, must also go through it). This is the most difficult
   element of the design.

## 3. What this does NOT solve

* Queries that wait on the server's own work (the service thread executes one request at a time, replay about 7 ms/frame) or on the global instance lock on the client side
  (`vkCreateImage` waited 69 ms for the lock; suspicion: the write producer waits for space in the send queue while holding it, see below).
* The CPU cost of compression (about 1-1.5 cores at 440-500 MB/s of raw writes) or video conversion.

Worth checking BEFORE building the channel: whether the write producer waits for space in the queue (`enqueue_item`, `space.wait`) **while holding the instance lock** (called from `send_mem` under
the lock). If so, then part of the 30-95 ms is the lock, not the TCP queue, and it is cheaper to move the wait before the lock (as was done with the frame throttle: `hamulec_klatek` 100 ms
held under the lock froze all threads).

## 4. Plan and order

0. **Instrumentation (done, 0.3.68):** histogram of query times with network round-trip (`czas zapytań z obiegiem po sieci: ...` in the ICD log, buckets <1, <2, <5, <10, <20, <50, <100, >=100 ms
   and p50/p95/p99/max). This is the "before" and "after" measure.
1. **Lock diagnosis (half a day):** a counter of time `enqueue_item` waiting for space and whether this happens under the instance lock; in spike dumps (`GPU_ICD_SPIKES`) separate
   "TCP queue" from "lock". The result decides whether the bulk channel is needed at all.
2. **Server:** bulk thread with channel `AttachBulk`, barrier `BulkBarrier`, `after_ctrl`; shared mirror; tests with a mock backend (like the `conn.rs` tests).
3. **Client:** writes on the bulk channel, u64 numbering, barriers before `QueueSubmit*` and reads, release, kill switch `GPU_ICD_BULK=0`, emergency fallback to the control stream
   after channel detach (after draining via barrier).
4. **Verification and load** (below).

Estimate: 2-4 days, half of which is mirror sharing and race tests.

## 5. Tests (required before deployment)

* Unit: ordering of writes on the bulk channel; the barrier waits exactly until `upto`; `after_ctrl` waits for allocation; number wraparound (u64, without the 16-bit window bug);
  channel break mid-flight (fallback to the control stream without losing a write).
* Race test proposed in consultation (DeepSeek, 2026-11-21): write A (0xAA) and a read on the second channel with dependency N, immediately write B (0xBB) on the main channel; the read must
  always return 0xAA, never 0xBB or a mixture; with random delays and many repetitions; deadlock test with many simultaneous reads and an intensive stream.
* Content verification mode (analogous to `GPU_ICD_RUNS_VERIFY`): every N-th frame the server compares GPU memory contents with the mirror after applying writes; any discrepancy is an error.
* Load: game start loop (`startloop.sh`, 40 starts, 0 Xid and 0 server exits), Cyberpunk and Tomb Raider runs without ordering errors (mirror discrepancies, Xid 109/32).

## 6. Success and failure criteria (measured before and after with the same tool)

Reference point (2026-11-22, Tomb Raider, adaptive zstd): Overall 44.3 FPS; scene 3: p95 46.6 ms, p99 63.6 ms, 12 frames > 60 ms out of 975; Syria: p99 61.7 ms, 11 out of 980;
query histogram with network round-trip, **Cyberpunk 2077** (0.3.68, one run, 31.75 FPS): 45 731 queries, p50 < 2 ms, p95 < 10 ms, p99 < 50 ms, max 449.7 ms; buckets
[<1, <2, <5, <10, <20, <50, <100, >=100 ms] = [15964, 7609, 15745, 4196, 1306, 877, 28, 6]. So 2% of queries (911 out of 45 731) take >= 20 ms; with an estimate of about 35 ms per such query
and about 3 ms for the rest, the tail is about 1/4 of the total query time. Histograms from Tomb Raider and Shadow of the Tomb Raider: to be collected in the next runs.

Success: (a) the number of frames > 60 ms drops by at least 30%; (b) query time p99 drops by at least 2x; (c) Overall does not drop by more than 1%; (d) 0 content discrepancies in verification
mode, 0 Xid in 40 starts; (e) no bulk channel exit outside the allowed one changes ordering relative to commands (race tests).
Failure (rollback, option `GPU_ICD_BULK=0` remains): (c) or (d) not met, or (a) and (b) without improvement, i.e. proof that the tail resulted from the lock or server work, not from the TCP queue.

## 7. Cheaper alternatives (to check before building the channel)

* Move the producer wait (`enqueue_item`) before the instance lock (backpressure before the lock): a small change, if the diagnosis from point 1 confirms the mechanism.
* Smaller queue (`GPU_ICD_SENDQ_MB=8`) tested 2026-11-21: no gain (40.8 vs 41.2 FPS), and fewer spikes (28 vs 45 frames > 60 ms) — inconclusive result from a single run;
  worth repeating with the query histogram, because now there is a way to evaluate them.
