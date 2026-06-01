#if defined(__clang__) || defined(__GNUC__)
#pragma GCC diagnostic ignored "-Wreturn-type-c-linkage"
#endif

#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

/// Bumped on any wire-type change. v2 added [`InitialInput::my_team_id`].
constexpr static const uint32_t ABI_VERSION = 2;

/// What kind of action a wizard is taking this tick. Together with the
/// numeric fields on [`WizardAction`] this is the full output schema.
enum class ActionKind : uint8_t {
  /// `MOVE x y thrust` — apply thrust toward (x, y); thrust in [0, 150].
  Move,
  /// `THROW x y power` — throw held snaffle toward (x, y) at power
  /// in [0, 500]. Ignored if wizard isn't holding a snaffle.
  Throw,
  /// `OBLIVIATE id` — bludger ignores caster's team for 4 turns. Cost 5.
  Obliviate,
  /// `PETRIFICUS id` — zero target velocity for 1 turn. Cost 10.
  Petrificus,
  /// `ACCIO id` — pull target toward caster for 6 turns. Cost 15.
  Accio,
  /// `FLIPENDO id` — push target away from caster for 3 turns. Cost 20.
  Flipendo,
};

/// Status byte returned by every bot's `take_turn` FFI call. Same shape for
/// every game — `Ok` means `TurnResult::output` is valid; `Panic` means the
/// bot's `catch_unwind` shim intercepted a panic and `output` is placeholder
/// data that the runner must ignore.
enum class BotStatus : uint8_t {
  Ok = 0,
  Panic = 1,
};

/// Tags for entities the engine emits per tick.
enum class EntityKind : uint8_t {
  /// One of the receiving player's own wizards (perspective-relative —
  /// the engine relabels per `input_for(player)`).
  Wizard,
  OpponentWizard,
  Snaffle,
  Bludger,
};

struct InitialInputFFI {
  int32_t my_team_id;
};

/// One wizard's action for one tick. Fields are kind-dependent — fill in
/// only the ones the kind needs; the rest are ignored on the wire.
///
/// Constructor helpers ([`WizardAction::move_to`], `throw_to`, `cast`)
/// build the right shape so callers don't need to remember which fields
/// each kind uses.
struct WizardAction {
  ActionKind kind;
  /// MOVE/THROW: target x. Spells: ignored.
  int32_t x;
  /// MOVE/THROW: target y. Spells: ignored.
  int32_t y;
  /// MOVE: thrust [0, 150]. THROW: power [0, 500]. Spells: ignored.
  int32_t power;
  /// Spells: target entity id. MOVE/THROW: ignored.
  int32_t target_id;
};

/// Output for one tick: the two actions for the player's two wizards, in
/// wizard-id order (lower id first). Written/read as two lines on the
/// wire — *not* `SingleLine`.
struct TurnOutput {
  WizardAction primary;
  WizardAction secondary;
};

/// FFI return type of every bot's `take_turn`. Generic over the per-game
/// `O` (the game's `TurnOutput`), monomorphised by cbindgen into a concrete
/// C++ struct per game.
template<typename O>
struct TurnResult {
  BotStatus status;
  O output;
};

/// One row of the per-tick entity list. `state` is kind-dependent:
///   * Wizard: `1` if grabbing a Snaffle, else `0`.
///   * Snaffle: `1` if grabbed by a Wizard, else `0`.
///   * Bludger: `entityId` of last victim (-1 if none).
struct Entity {
  int32_t id;
  EntityKind kind;
  int32_t x;
  int32_t y;
  int32_t vx;
  int32_t vy;
  int32_t state;
};

/// `#[repr(C)]` FFI mirror of [`TurnInput`]. Fields are private — the only
/// way to obtain a `TurnInputFFI<'a>` is via `TurnInput::as_ffi`, which
/// establishes the invariants `as_ref` relies on:
///   1. `entities` is a valid, properly-aligned pointer to a contiguous
///      array of `Entity`s.
///   2. The array has at least `num_entities` elements.
///   3. The memory is live for `'a` (enforced by lifetime + PhantomData).
struct TurnInputFFI {
  int32_t my_score;
  int32_t my_magic;
  int32_t opp_score;
  int32_t opp_magic;
  const Entity *entities;
  uintptr_t num_entities;
};

extern "C" {

extern void initialize(InitialInputFFI input);

extern TurnResult<TurnOutput> take_turn(TurnInputFFI input);

extern uint32_t abi_version();

}  // extern "C"
