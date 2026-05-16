---- MODULE agent_mail_coordination ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Agents, Paths, MaxStep

VARIABLES reservations, step

Reservation == [agent: Agents, path: Paths, exclusive: BOOLEAN]

Init ==
  /\ reservations = {}
  /\ step = 0

Overlaps(a, b) ==
  /\ a.path = b.path
  /\ a.agent # b.agent
  /\ a.exclusive
  /\ b.exclusive

CanGrant(candidate) ==
  \A existing \in reservations : ~Overlaps(candidate, existing)

Grant(candidate) ==
  /\ candidate \in Reservation
  /\ CanGrant(candidate)
  /\ reservations' = reservations \cup {candidate}
  /\ step' = step + 1

Release(candidate) ==
  /\ candidate \in reservations
  /\ reservations' = reservations \ {candidate}
  /\ step' = step + 1

Stutter ==
  /\ reservations' = reservations
  /\ step' = step + 1

Next ==
  /\ step < MaxStep
  /\ \/ \E candidate \in Reservation : Grant(candidate)
     \/ \E candidate \in reservations : Release(candidate)
     \/ Stutter

\* invariant: exclusive_reservations_do_not_overlap
NoOverlap ==
  \A left \in reservations :
    \A right \in reservations :
      left = right \/ ~Overlaps(left, right)

====
