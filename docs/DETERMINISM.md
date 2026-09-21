# Determinism contract

Given identical recorded initialization and ordered commands, authority, replicas,
and replay must produce identical authoritative state at the same tick.

## Game responsibilities

- Construct the world solely from recorded initialization, including seeds and
  scenario choices. Wall-clock time, ambient randomness, and local files must not
  influence authoritative rules.
- Apply commands in framework order and reject illegal gameplay deterministically.
  Identity is a session participant, not an authenticated account.
- Order dependent simulation systems explicitly. Bevy scheduling ambiguity
  detection is not currently enabled by Synctick; see the
  [ordering guidance](SESSION_FRAMEWORK.md#simulation-system-ordering-and-ambiguity-detection).
- Iterate authoritative collections in a stable order. Sort ECS entities by a
  game-owned logical ID; Bevy entity allocation and query order are not the contract.
- Include all future-affecting state in hashes: tick, resources, components,
  allocation counters, and seeded RNG state. Keep presentation effects and
  recomputable caches outside derived authoritative state.
- Use deterministic arithmetic. Integer overflow policy must be explicit. Games
  using floats must establish portable evaluation, including transcendental
  functions and fused operations; stable hashing does not make arithmetic portable.

## Stable hashes

`StableHash` derives include all fields unless marked `#[stable_hash(skip)]`.
Skipped fields must be presentation data or deterministically recomputable caches;
they contribute no bytes, and differences in them cannot trigger desync detection. The game selects state and ordering;
`StateHasher` defines fixed FNV-1a 64-bit mixing and canonical representations.
Integers are fixed-width little-endian; floats preserve exact bits; sequences
and UTF-8 strings are length-prefixed. Enum variants have explicit byte tags.
See the [hashing guide](SESSION_FRAMEWORK.md#stable-state-hashing) for the full
contract, composition examples, and unsupported types.

A matching noncryptographic fingerprint is evidence, not proof, of identical
state. It cannot catch omitted authoritative fields. Changing representations,
field order, tags, rules, or the hash algorithm requires a game compatibility
version bump. Package renaming alone changes none of these.

## Framework safeguards and limits

The framework checks tick continuity, bounded payloads, compatible startup,
readiness, and periodic hash reports. Cadences derive from the fixed recorded tick
duration. It preserves participant origin through broadcasting and replay.
Transient effects are cleaned after each attempted tick, including replay;
panics terminate the worker without guaranteed cleanup.

Tests cover hash/codec golden bytes, malformed traffic, packet loss, startup
readiness, cancellation, recording failures, and example host/client/replay
parity. CI runs on Ubuntu only. Sustained sessions between
heterogeneous operating systems and CPUs remain an additional validation task;
local passing tests alone do not establish cross-platform determinism.

Late joining, reconnecting, prediction, rollback, runtime speed changes, and
presentation-clock correction are outside this extraction. No new schedule
ambiguity rejection policy is introduced.
