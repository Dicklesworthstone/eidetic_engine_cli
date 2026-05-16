import Std

namespace Ee.Proofs.PackDeterminism

structure PackInputs where
  canonicalHash : String
deriving Repr, DecidableEq

structure PackOutput where
  hash : String
deriving Repr, DecidableEq

def pack_assemble (inputs : PackInputs) : PackOutput :=
  { hash := inputs.canonicalHash }

def hash (output : PackOutput) : String :=
  output.hash

def canonical_hash (inputs : PackInputs) : String :=
  inputs.canonicalHash

-- invariant: pack_determinism
theorem pack_determinism :
    forall inputs : PackInputs, hash (pack_assemble inputs) = canonical_hash inputs := by
  intro inputs
  rfl

end Ee.Proofs.PackDeterminism

