#!/usr/bin/env python3
"""bd-tc-epic-qzk7o.8.7 — deterministic fake OIDC IdP for team-confed tier-2 tests.

Serves TLS discovery/JWKS/device/token endpoints on 127.0.0.1 with an
ephemeral CA, a scriptable RFC 8628 device-flow state machine, and RS256 /
ES256 ID-token minting with rotatable keys. Test-only capability, poll,
identity-time, lease, bootstrap, transient-frame, and privacy-projection
oracles are driven over the same HTTPS endpoint. Zero third-party python
dependencies: signing and TLS use the system `openssl` binary; the server is
http.server + ssl from the stdlib. Never touches the real network.

Usage:
  fake_idp.py --dir <state-dir> --port <port> [--scenario <scenario.json>]

The state dir receives: ca.pem (for clients to trust), server cert/key,
signing keys, ready file (port written once listening), and a control log.
Test drivers mutate behavior at runtime via POST /_control and inspect via
GET /_state. Restarting in the same state directory preserves the secret-free
identity floor/outer state and scrubbed evidence, but drops all in-memory
device, poll, and transient-frame state so a fresh ceremony is required.
"""

import argparse
import base64
import copy
import hashlib
import hmac
import ipaddress
import json
import os
import select
import signal
import ssl
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


MAX_U64 = (1 << 64) - 1
IDENTITY_MUTATING_PATHS = {
    "token_verify",
    "idp_set",
    "grant",
    "serve",
    "sync_import",
    "steward",
    "revalidate",
}
IDENTITY_READ_ONLY_PATHS = {"status", "doctor", "activity", "audit"}
CAPABILITY_PROFILES = {
    "absent": [],
    "manifest_only": ["mesh.team.manifest.v1"],
    "identity_attested": [
        "mesh.team.identity_attested.v1",
        "mesh.team.manifest.v1",
    ],
}
JWKS_PROFILES = {
    "normal",
    "rsa_1024",
    "rsa_bad_exponent",
    "ec_wrong_curve",
    "missing_kid",
    "duplicate_same_kid",
    "ambiguous_same_kid",
    "metadata_mismatch",
    "zero_eligible",
}


def atomic_write_json(path: str, payload: dict) -> None:
    temporary = f"{path}.tmp"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
    os.replace(temporary, path)


class ScenarioClock:
    """Injectable wall/monotonic clock used only by the offline harness."""

    def __init__(self, config: dict):
        self.virtual = bool(config)
        self.wall = int(config.get("wall", 1_700_000_000)) if self.virtual else 0
        self.monotonic = int(config.get("monotonic", 0)) if self.virtual else 0

    def wall_time(self) -> float:
        return float(self.wall) if self.virtual else time.time()

    def monotonic_time(self) -> float:
        return float(self.monotonic) if self.virtual else time.monotonic()

    def set(self, wall=None, monotonic=None) -> None:
        if not self.virtual:
            raise ValueError("logical clock is not enabled for this scenario")
        if wall is not None:
            self.wall = int(wall)
        if monotonic is not None:
            self.monotonic = int(monotonic)

    def advance(self, wall=0, monotonic=0) -> None:
        if not self.virtual:
            raise ValueError("logical clock is not enabled for this scenario")
        self.wall += int(wall)
        self.monotonic += int(monotonic)

    def snapshot(self) -> dict:
        return {
            "virtual": self.virtual,
            "wall": int(self.wall_time()),
            "monotonic": int(self.monotonic_time()),
        }


class NetworkOracle:
    """Pure URL/DNS/redirect oracle consumed by the offline client tests."""

    @staticmethod
    def _strict_url(value: str):
        try:
            parsed = urlsplit(value)
            port = parsed.port or 443
        except (TypeError, ValueError):
            return None, "invalid_url"
        if parsed.scheme != "https":
            return None, "https_required"
        if not parsed.hostname:
            return None, "host_required"
        if parsed.username is not None or parsed.password is not None:
            return None, "userinfo_forbidden"
        if parsed.fragment:
            return None, "fragment_forbidden"
        return (parsed.hostname.lower(), port), None

    @staticmethod
    def _address_forbidden(address) -> bool:
        return any(
            (
                address.is_private,
                address.is_loopback,
                address.is_link_local,
                address.is_multicast,
                address.is_reserved,
                address.is_unspecified,
            )
        )

    def evaluate(self, spec: dict) -> dict:
        origin, error = self._strict_url(spec.get("url", ""))
        if error:
            return {"status": "rejected", "reason": error}
        validation = list(spec.get("validation_ips", []))
        presentation = list(spec.get("presentation_ips", validation))
        if not validation or not presentation:
            return {"status": "rejected", "reason": "dns_answer_required"}
        try:
            validation_addresses = [ipaddress.ip_address(value) for value in validation]
            presentation_addresses = [
                ipaddress.ip_address(value) for value in presentation
            ]
        except (TypeError, ValueError):
            return {"status": "rejected", "reason": "invalid_dns_address"}
        if sorted({str(value) for value in validation_addresses}) != sorted(
            {str(value) for value in presentation_addresses}
        ):
            return {"status": "rejected", "reason": "dns_rebinding"}
        if not spec.get("private_approved", False) and any(
            self._address_forbidden(value) for value in presentation_addresses
        ):
            return {"status": "rejected", "reason": "private_address_unapproved"}
        redirects = list(spec.get("redirects", []))
        if len(redirects) > 5:
            return {"status": "rejected", "reason": "redirect_limit"}
        if spec.get("credentialed_post", False) and redirects:
            return {"status": "rejected", "reason": "credential_post_redirect"}
        for redirect in redirects:
            target_origin, redirect_error = self._strict_url(redirect)
            if redirect_error:
                return {"status": "rejected", "reason": redirect_error}
            if target_origin != origin:
                return {"status": "rejected", "reason": "cross_origin_redirect"}
        return {
            "status": "allowed",
            "origin": {"host": origin[0], "port": origin[1]},
            "pinnedAddresses": sorted({str(value) for value in presentation_addresses}),
            "redirectCount": len(redirects),
        }


class PollOracle:
    """Deterministic RFC 8628 scheduling oracle for future client tests."""

    TERMINAL = {"success", "cancelled", "terminal_error", "expired", "invalid"}

    def __init__(self):
        self.reset()

    def reset(self) -> None:
        self.status = "unconfigured"
        self.validation_error = None
        self.start = None
        self.provider_deadline = None
        self.local_deadline = None
        self.deadline = None
        self.deadline_reason = None
        self.interval = None
        self.next_poll_at = None
        self.request_count = 0
        self.timeout_backoff = 0
        self.expiry_reason = None
        self.restart_required = False
        self.last_observed_at = None
        self.trace = []

    @staticmethod
    def _positive_u64(name: str, value):
        if isinstance(value, bool) or not isinstance(value, int):
            return None, f"{name}_not_integer"
        if value <= 0:
            return None, f"{name}_not_positive"
        if value > MAX_U64:
            return None, f"{name}_overflow"
        return value, None

    def configure(self, response: dict, start: int) -> dict:
        self.reset()
        if "expires_in" not in response or response.get("expires_in") is None:
            self.status = "invalid"
            self.validation_error = "expires_in_missing"
            self.restart_required = True
            return self.snapshot()
        expires_in, error = self._positive_u64(
            "expires_in", response.get("expires_in")
        )
        if error:
            self.status = "invalid"
            self.validation_error = error
            self.restart_required = True
            return self.snapshot()
        if "interval" in response:
            interval, error = self._positive_u64("interval", response.get("interval"))
            if error:
                self.status = "invalid"
                self.validation_error = error
                self.restart_required = True
                return self.snapshot()
        else:
            interval = 5
        if start < 0 or start > MAX_U64:
            self.status = "invalid"
            self.validation_error = "monotonic_start_out_of_range"
            self.restart_required = True
            return self.snapshot()
        if expires_in > MAX_U64 - start or 1800 > MAX_U64 - start:
            self.status = "invalid"
            self.validation_error = "deadline_overflow"
            self.restart_required = True
            return self.snapshot()
        self.start = start
        self.provider_deadline = start + expires_in
        self.local_deadline = start + 1800
        if self.provider_deadline <= self.local_deadline:
            self.deadline = self.provider_deadline
            self.deadline_reason = "provider_deadline"
        else:
            self.deadline = self.local_deadline
            self.deadline_reason = "local_deadline"
        self.interval = interval
        self.last_observed_at = start
        if interval > MAX_U64 - start:
            self.status = "invalid"
            self.validation_error = "next_poll_overflow"
            self.restart_required = True
            return self.snapshot()
        self.next_poll_at = start + interval
        self.status = "waiting"
        if self.next_poll_at > self.deadline:
            self._expire(self.deadline_reason)
        return self.snapshot()

    def _expire(self, reason: str) -> None:
        self.status = "expired"
        self.expiry_reason = reason
        self.restart_required = True

    def _schedule(self, now: int, delay: int) -> bool:
        if delay > MAX_U64 - now:
            self.status = "terminal_error"
            self.validation_error = "wait_overflow"
            self.restart_required = True
            return False
        candidate = now + delay
        if candidate > self.deadline:
            self._expire(self.deadline_reason)
            return False
        self.next_poll_at = candidate
        return True

    def attempt(self, event: str, now: int) -> dict:
        if self.status in self.TERMINAL:
            return self.snapshot()
        if self.status == "unconfigured":
            self.status = "invalid"
            self.validation_error = "poll_not_configured"
            self.restart_required = True
            return self.snapshot()
        if now < 0 or now > MAX_U64:
            self.status = "terminal_error"
            self.validation_error = "monotonic_now_out_of_range"
            self.restart_required = True
            return self.snapshot()
        if self.last_observed_at is not None and now < self.last_observed_at:
            self.status = "terminal_error"
            self.validation_error = "monotonic_clock_rollback"
            self.restart_required = True
            return self.snapshot()
        self.last_observed_at = now
        if event == "cancelled":
            self.status = "cancelled"
            self.restart_required = True
            self.trace.append(
                {"event": event, "at": now, "requestSent": False, "status": self.status}
            )
            return self.snapshot()
        if now >= self.deadline:
            self._expire(self.deadline_reason)
            self.trace.append(
                {"event": event, "at": now, "requestSent": False, "status": self.status}
            )
            return self.snapshot()
        if now < self.next_poll_at:
            self.trace.append(
                {"event": "early_poll", "at": now, "requestSent": False, "status": self.status}
            )
            return self.snapshot()
        if self.request_count >= 300:
            self._expire("request_budget")
            return self.snapshot()

        self.request_count += 1
        row = {
            "event": event,
            "at": now,
            "requestSent": True,
            "requestCount": self.request_count,
            "interval": self.interval,
        }
        self.trace.append(row)

        if self.request_count >= 300 and event in {
            "authorization_pending",
            "slow_down",
            "timeout",
        }:
            self._expire("request_budget")
        elif event == "authorization_pending":
            self.timeout_backoff = 0
            self._schedule(now, self.interval)
        elif event == "slow_down":
            if self.interval > MAX_U64 - 5:
                self.status = "terminal_error"
                self.validation_error = "interval_overflow"
                self.restart_required = True
            else:
                self.interval += 5
                self.timeout_backoff = 0
                self._schedule(now, self.interval)
        elif event == "timeout":
            if self.timeout_backoff == 0:
                self.timeout_backoff = 1
            elif self.timeout_backoff > MAX_U64 // 2:
                self.status = "terminal_error"
                self.validation_error = "timeout_backoff_overflow"
                self.restart_required = True
            else:
                self.timeout_backoff *= 2
            if self.status == "waiting":
                self._schedule(now, max(self.interval, self.timeout_backoff))
        elif event == "granted":
            self.status = "success"
        elif event == "expired_token":
            self._expire("provider_error")
        elif event in {"access_denied", "malformed", "unknown_error"}:
            self.status = "terminal_error"
            self.validation_error = event
            self.restart_required = True
        else:
            self.status = "terminal_error"
            self.validation_error = "unsupported_poll_event"
            self.restart_required = True
        return self.snapshot()

    def repeat_legal(self, event: str, count: int, clock: ScenarioClock) -> dict:
        count = int(count)
        if count < 0 or count > 300:
            self.status = "terminal_error"
            self.validation_error = "repeat_count_out_of_range"
            self.restart_required = True
            return self.snapshot()
        for _ in range(count):
            if self.status in self.TERMINAL:
                break
            if not clock.virtual:
                raise ValueError("repeat_legal requires a logical clock")
            if clock.monotonic < self.next_poll_at:
                clock.monotonic = self.next_poll_at
            self.attempt(event, int(clock.monotonic))
        return self.snapshot()

    def snapshot(self) -> dict:
        return {
            "status": self.status,
            "validationError": self.validation_error,
            "start": self.start,
            "providerDeadline": self.provider_deadline,
            "localDeadline": self.local_deadline,
            "deadline": self.deadline,
            "deadlineReason": self.deadline_reason,
            "interval": self.interval,
            "nextPollAt": self.next_poll_at,
            "requestCount": self.request_count,
            "timeoutBackoff": self.timeout_backoff,
            "expiryReason": self.expiry_reason,
            "restartRequired": self.restart_required,
            "lastObservedAt": self.last_observed_at,
            "trace": copy.deepcopy(self.trace),
        }


class IdentityOracle:
    """Durable, secret-free reference model for T7.3/T7.6 acceptance tests."""

    def __init__(self, state_dir: str, initial_floor: int):
        self.path = os.path.join(state_dir, "identity-oracle.json")
        if os.path.exists(self.path):
            with open(self.path, "r", encoding="utf-8") as handle:
                saved = json.load(handle)
        else:
            saved = {}
        self.floor = int(saved.get("floor", initial_floor))
        self.audit_count = int(saved.get("auditCount", 0))
        self.effect_count = int(saved.get("effectCount", 0))
        self.leases = list(saved.get("leases", []))
        self.replay_claims = dict(saved.get("replayClaims", {}))
        self.conflicts = list(saved.get("conflicts", []))
        self.outer_state = saved.get("outerState", "identity_pending")
        self.renewals = dict(saved.get("renewals", {}))
        self.bootstrap = saved.get(
            "bootstrap",
            {
                "tier2Enabled": False,
                "state": "off",
                "policyGeneration": 0,
                "members": ["creator"],
                "attested": [],
                "identities": {},
                "graceDeadline": None,
                "graceSeconds": 0,
                "ceremonyEpoch": 0,
                "backgroundIdpRequests": 0,
            },
        )
        self.persist()

    def persist(self) -> None:
        atomic_write_json(
            self.path,
            {
                "floor": self.floor,
                "auditCount": self.audit_count,
                "effectCount": self.effect_count,
                "leases": self.leases,
                "replayClaims": self.replay_claims,
                "conflicts": self.conflicts,
                "outerState": self.outer_state,
                "renewals": self.renewals,
                "bootstrap": self.bootstrap,
            },
        )

    def eligible_leases(self, at_time=None, subject_member=None) -> list:
        generation = self.bootstrap.get("policyGeneration", 0)
        effective = self.floor if at_time is None else int(at_time)
        return sorted(
            [
                lease
                for lease in self.leases
                if not lease.get("suppressed", False)
                and lease.get("policyGeneration") == generation
                and lease.get("validUntil", 0) > effective
                and (
                    subject_member is None
                    or lease.get("subjectMember") == subject_member
                )
            ],
            key=lambda lease: lease["eventHash"],
        )

    def conflict_sets(self, at_time=None) -> list:
        grouped = {}
        generation = self.bootstrap.get("policyGeneration", 0)
        effective = self.floor if at_time is None else int(at_time)
        for lease in self.leases:
            if (
                lease.get("suppressed", False)
                or lease.get("policyGeneration") != generation
                or lease.get("validUntil", 0) <= effective
            ):
                continue
            key = (
                lease.get("issuer"),
                lease.get("subject"),
                lease.get("policyGeneration"),
            )
            grouped.setdefault(key, set()).add(lease.get("subjectMember"))
        return sorted(
            sorted(member for member in members if member is not None)
            for members in grouped.values()
            if len(members) > 1
        )

    def snapshot(self, wall: int) -> dict:
        effective = max(self.floor, int(wall))
        eligible = self.eligible_leases(effective)
        subjects = sorted(
            {
                lease.get("subjectMember")
                for lease in self.leases
                if lease.get("subjectMember") is not None
            }
        )
        eligible_sets = {
            subject: [
                row["id"]
                for row in self.eligible_leases(effective, subject_member=subject)
            ]
            for subject in subjects
        }
        effective_deadlines = {
            subject: max(
                (
                    row["validUntil"]
                    for row in self.eligible_leases(
                        effective, subject_member=subject
                    )
                ),
                default=None,
            )
            for subject in subjects
        }
        conflicts = self.conflict_sets(effective)
        return {
            "floor": self.floor,
            "effectiveTime": effective,
            "auditCount": self.audit_count,
            "effectCount": self.effect_count,
            "outerState": self.outer_state,
            "leases": copy.deepcopy(sorted(self.leases, key=lambda row: row["eventHash"])),
            "eligibleLeaseIds": [row["id"] for row in eligible],
            "effectiveDeadline": max(
                (row["validUntil"] for row in eligible), default=None
            ),
            "eligibleLeaseSets": eligible_sets,
            "effectiveDeadlines": effective_deadlines,
            "conflicts": conflicts,
            "manifestStatus": "manifest_conflict" if conflicts else "consistent",
            "paused": bool(conflicts),
            "renewals": copy.deepcopy(self.renewals),
            "bootstrap": copy.deepcopy(self.bootstrap),
        }

    def reset(self, floor: int) -> None:
        self.floor = int(floor)
        self.audit_count = 0
        self.effect_count = 0
        self.leases = []
        self.replay_claims = {}
        self.conflicts = []
        self.outer_state = "identity_pending"
        self.renewals = {}
        self.bootstrap = {
            "tier2Enabled": False,
            "state": "off",
            "policyGeneration": 0,
            "members": ["creator"],
            "attested": [],
            "identities": {},
            "graceDeadline": None,
            "graceSeconds": 0,
            "ceremonyEpoch": 0,
            "backgroundIdpRequests": 0,
        }
        self.persist()

    def evaluate_path(self, name: str, wall: int) -> dict:
        before = (self.floor, self.audit_count, self.effect_count)
        if name in IDENTITY_READ_ONLY_PATHS:
            return {
                "status": "read_only",
                "path": name,
                "effectiveTime": max(self.floor, int(wall)),
                "persistedUnchanged": before
                == (self.floor, self.audit_count, self.effect_count),
            }
        if name not in IDENTITY_MUTATING_PATHS:
            return {"status": "invalid_path", "path": name}
        if int(wall) < self.floor:
            return {
                "status": "team_identity_clock_rollback",
                "path": name,
                "floor": self.floor,
                "effectsUnchanged": before
                == (self.floor, self.audit_count, self.effect_count),
            }
        self.floor = max(self.floor, int(wall))
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {"status": "authorized", "path": name, "floor": self.floor}

    def admit_lease(
        self, spec: dict, wall: int, *, allow_bootstrap: bool = False
    ) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        effective = max(self.floor, int(wall))
        required = [
            "subjectMember",
            "verifierMember",
            "issuer",
            "subject",
            "policyGeneration",
            "verifiedAt",
            "validUntil",
            "evidenceExpiry",
            "policyCadence",
        ]
        missing = [name for name in required if name not in spec]
        if missing:
            return {"status": "rejected", "reason": "missing_fields", "missing": missing}
        supplied_event_hash = spec.get("eventHash")
        if supplied_event_hash is not None:
            duplicate_event = next(
                (
                    row
                    for row in self.leases
                    if row.get("eventHash") == supplied_event_hash
                ),
                None,
            )
            if duplicate_event is not None:
                return {
                    "status": "duplicate",
                    "leaseId": duplicate_event["id"],
                    "effectsUnchanged": True,
                }
        if not spec.get("verifierActive", True) or not spec.get("verifierNodeActive", True):
            return {"status": "rejected", "reason": "verifier_inactive"}
        if spec["subjectMember"] == spec["verifierMember"]:
            return {"status": "rejected", "reason": "self_attestation"}
        if spec["policyGeneration"] != self.bootstrap.get("policyGeneration", 0):
            return {"status": "rejected", "reason": "policy_generation_mismatch"}
        bootstrap_exception = bool(spec.get("bootstrapException", False))
        if bootstrap_exception:
            exception_allowed = (
                allow_bootstrap
                and self.bootstrap.get("tier2Enabled", False)
                and self.bootstrap.get("state") == "grace"
                and spec["verifierMember"] in self.bootstrap.get("members", [])
            )
            if not exception_allowed:
                return {
                    "status": "rejected",
                    "reason": "bootstrap_exception_not_allowed",
                }
        else:
            verifier_leases = [
                row
                for row in self.eligible_leases(effective)
                if row.get("subjectMember") == spec["verifierMember"]
            ]
            if not verifier_leases:
                return {
                    "status": "rejected",
                    "reason": "verifier_missing_current_lease",
                }
            if any(
                row.get("issuer") == spec["issuer"]
                and row.get("subject") == spec["subject"]
                for row in verifier_leases
            ):
                return {"status": "rejected", "reason": "same_subject_verifier"}
        verified_at = int(spec["verifiedAt"])
        valid_until = int(spec["validUntil"])
        evidence_expiry = int(spec["evidenceExpiry"])
        cadence = int(spec["policyCadence"])
        if verified_at > effective + 600:
            return {"status": "rejected", "reason": "verified_at_future_skew"}
        if valid_until <= verified_at:
            return {"status": "rejected", "reason": "nonpositive_duration"}
        if valid_until > verified_at + cadence:
            return {"status": "rejected", "reason": "policy_cadence_exceeded"}
        if valid_until > evidence_expiry:
            return {"status": "rejected", "reason": "evidence_expiry_exceeded"}
        if valid_until <= effective:
            return {"status": "rejected", "reason": "already_expired"}

        event_hash = spec.get("eventHash") or hashlib.sha256(
            json.dumps(spec, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        lease = {
            "id": spec.get("id", f"lease-{event_hash[:12]}"),
            "eventHash": event_hash,
            "subjectMember": spec["subjectMember"],
            "verifierMember": spec["verifierMember"],
            "issuer": spec["issuer"],
            "subject": spec["subject"],
            "policyGeneration": spec["policyGeneration"],
            "verifiedAt": verified_at,
            "validUntil": valid_until,
            "evidenceExpiry": evidence_expiry,
            "suppressed": False,
        }
        duplicate_identity = next(
            (
                row
                for row in self.leases
                if row["issuer"] == lease["issuer"]
                and row["subject"] == lease["subject"]
                and row["policyGeneration"] == lease["policyGeneration"]
                and row["subjectMember"] != lease["subjectMember"]
                and not row.get("suppressed", False)
                and row.get("validUntil", 0) > effective
            ),
            None,
        )
        self.leases.append(lease)
        self.renewals[lease["subjectMember"]] = {
            "subjectMember": lease["subjectMember"],
            "state": "active",
            "dueAt": valid_until,
            "graceDeadline": None,
            "verifierAvailable": True,
            "backgroundIdpRequests": 0,
        }
        self.floor = effective
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {
            "status": "conflict" if duplicate_identity else "accepted",
            "leaseId": lease["id"],
            "effectiveDeadline": self.snapshot(wall)["effectiveDeadlines"].get(
                lease["subjectMember"]
            ),
        }

    def repair_floor(self, new_floor: int, confirmed: bool, corrected: bool) -> dict:
        if not confirmed:
            return {"status": "confirmation_required"}
        if not corrected:
            return {"status": "clock_not_corrected"}
        suppressed = []
        for lease in self.leases:
            if not lease.get("suppressed", False):
                lease["suppressed"] = True
                suppressed.append(lease["id"])
        self.floor = int(new_floor)
        self.outer_state = "identity_pending"
        repair_grace_seconds = max(
            1, int(self.bootstrap.get("graceSeconds", 600))
        )
        self.bootstrap["state"] = (
            "grace" if self.bootstrap.get("tier2Enabled", False) else "pending"
        )
        self.bootstrap["attested"] = []
        self.bootstrap["ceremonyEpoch"] = (
            int(self.bootstrap.get("ceremonyEpoch", 0)) + 1
        )
        self.bootstrap["graceDeadline"] = (
            self.floor + repair_grace_seconds
            if self.bootstrap.get("tier2Enabled", False)
            else None
        )
        self.renewals = {
            member: {
                "subjectMember": member,
                "state": "pending",
                "dueAt": None,
                "graceDeadline": self.bootstrap["graceDeadline"],
                "verifierAvailable": False,
                "backgroundIdpRequests": 0,
            }
            for member in self.bootstrap.get("members", [])
        }
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {
            "status": "repaired",
            "floor": self.floor,
            "suppressedLeaseIds": sorted(suppressed),
        }

    def replay_claim(
        self,
        issuer: str,
        client_id: str,
        claim: str,
        claimant: str,
        expires_at: int,
        wall: int,
    ) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        self.floor = max(self.floor, int(wall))
        self.audit_count += 1
        if int(expires_at) <= int(wall):
            self.persist()
            return {"status": "rejected", "reason": "token_expired"}
        scope = {
            "issuer": issuer,
            "clientId": client_id,
            "claim": claim,
        }
        claim_key = hashlib.sha256(
            json.dumps(scope, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ).hexdigest()
        record = self.replay_claims.get(claim_key)
        if isinstance(record, dict) and int(record.get("expiresAt", 0)) > int(wall):
            self.persist()
            return {
                "status": "replay",
                "winner": record["winner"],
                "claimHash": claim_key,
                "expiresAt": record["expiresAt"],
            }
        self.replay_claims[claim_key] = {
            "winner": claimant,
            "expiresAt": int(expires_at),
        }
        self.effect_count += 1
        self.persist()
        return {
            "status": "accepted",
            "winner": claimant,
            "claimHash": claim_key,
            "expiresAt": int(expires_at),
        }

    def bootstrap_enable(self, generation: int, grace_seconds: int, wall: int) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        generation = int(generation)
        grace_seconds = int(grace_seconds)
        if grace_seconds < 0:
            return {"status": "rejected", "reason": "negative_grace"}
        current_generation = int(self.bootstrap.get("policyGeneration", 0))
        if generation <= current_generation:
            return {
                "status": "rejected",
                "reason": "policy_generation_not_advanced",
            }
        if len(self.bootstrap.get("members", [])) == 1 and grace_seconds == 0:
            return {"status": "zero_grace_refused"}
        for lease in self.leases:
            if (
                not lease.get("suppressed", False)
                and lease.get("policyGeneration") != generation
            ):
                lease["suppressed"] = True
        self.renewals = {
            member: {
                "subjectMember": member,
                "state": "pending",
                "dueAt": None,
                "graceDeadline": int(wall) + grace_seconds,
                "verifierAvailable": False,
                "backgroundIdpRequests": 0,
            }
            for member in self.bootstrap.get("members", [])
        }
        self.floor = max(self.floor, int(wall))
        self.bootstrap.update(
            {
                "tier2Enabled": True,
                "state": "grace" if grace_seconds > 0 else "pending",
                "policyGeneration": generation,
                "attested": [],
                "graceDeadline": int(wall) + grace_seconds,
                "graceSeconds": grace_seconds,
                "ceremonyEpoch": 0,
                "backgroundIdpRequests": 0,
            }
        )
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {"status": "enabled", "bootstrap": copy.deepcopy(self.bootstrap)}

    def bootstrap_verify(
        self,
        subject_member: str,
        verifier_member: str,
        subject_issuer: str,
        subject: str,
        verifier_issuer: str,
        verifier_subject: str,
        lease_seconds: int,
        wall: int,
    ) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        if not self.bootstrap.get("tier2Enabled", False):
            return {"status": "rejected", "reason": "tier2_not_enabled"}
        if self.bootstrap.get("state") != "grace":
            return {"status": "rejected", "reason": "bootstrap_exception_closed"}
        grace_deadline = self.bootstrap.get("graceDeadline")
        if grace_deadline is None:
            return {"status": "rejected", "reason": "bootstrap_deadline_missing"}
        if int(wall) >= int(grace_deadline):
            self.floor = max(self.floor, int(wall))
            self.bootstrap["state"] = "suspended"
            for renewal in self.renewals.values():
                renewal["state"] = "suspended"
            self.audit_count += 1
            self.effect_count += 1
            self.persist()
            return {"status": "rejected", "reason": "bootstrap_grace_expired"}
        if subject_member == verifier_member:
            return {"status": "rejected", "reason": "self_attestation"}
        if (subject_issuer, subject) == (verifier_issuer, verifier_subject):
            return {"status": "rejected", "reason": "same_subject_verifier"}
        if verifier_member not in self.bootstrap.get("members", []):
            return {"status": "rejected", "reason": "verifier_not_active"}
        identities = self.bootstrap.setdefault("identities", {})
        supplied_identities = {
            subject_member: {"issuer": subject_issuer, "subject": subject},
            verifier_member: {
                "issuer": verifier_issuer,
                "subject": verifier_subject,
            },
        }
        if any(
            member in identities and identities[member] != identity
            for member, identity in supplied_identities.items()
        ):
            return {"status": "rejected", "reason": "member_identity_mismatch"}
        lease_seconds = int(lease_seconds)
        if lease_seconds <= 0 or lease_seconds > 86_400:
            return {"status": "rejected", "reason": "bootstrap_lease_out_of_bounds"}
        generation = int(self.bootstrap["policyGeneration"])
        ceremony_epoch = int(self.bootstrap.get("ceremonyEpoch", 0))
        lease_suffix = (
            f"{generation}-{subject_member}"
            if ceremony_epoch == 0
            else f"{generation}-r{ceremony_epoch}-{subject_member}"
        )
        lease_result = self.admit_lease(
            {
                "id": f"bootstrap-{lease_suffix}",
                "eventHash": hashlib.sha256(
                    f"{generation}:{ceremony_epoch}:{subject_member}:{verifier_member}:{subject_issuer}:{subject}".encode(
                        "utf-8"
                    )
                ).hexdigest(),
                "subjectMember": subject_member,
                "verifierMember": verifier_member,
                "issuer": subject_issuer,
                "subject": subject,
                "policyGeneration": generation,
                "verifiedAt": int(wall),
                "validUntil": int(wall) + lease_seconds,
                "evidenceExpiry": int(wall) + lease_seconds,
                "policyCadence": lease_seconds,
                "bootstrapException": True,
            },
            int(wall),
            allow_bootstrap=True,
        )
        if lease_result.get("status") not in {"accepted", "duplicate"}:
            return lease_result
        identities.update(supplied_identities)
        if subject_member not in self.bootstrap["members"]:
            self.bootstrap["members"].append(subject_member)
            self.bootstrap["members"].sort()
        if subject_member not in self.bootstrap["attested"]:
            self.bootstrap["attested"].append(subject_member)
            self.bootstrap["attested"].sort()
        if set(self.bootstrap["attested"]) >= set(self.bootstrap["members"]):
            self.bootstrap["state"] = "active"
            self.outer_state = "active"
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {
            "status": "verified",
            "leaseId": lease_result.get("leaseId"),
            "bootstrap": copy.deepcopy(self.bootstrap),
        }

    def bootstrap_tick(self, wall: int) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        self.floor = max(self.floor, int(wall))
        deadline = self.bootstrap.get("graceDeadline")
        current_state = self.bootstrap.get("state")
        if current_state not in {"active", "suspended"} and deadline is not None:
            if int(wall) >= int(deadline):
                self.bootstrap["state"] = "suspended"
            else:
                self.bootstrap["state"] = "grace"
            for renewal in self.renewals.values():
                renewal["state"] = (
                    "suspended"
                    if self.bootstrap["state"] == "suspended"
                    else "pending"
                )
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {"status": self.bootstrap["state"], "bootstrap": copy.deepcopy(self.bootstrap)}

    def renewal_tick(
        self,
        subject_member: str,
        wall: int,
        grace_seconds: int,
        verifier_available: bool,
    ) -> dict:
        if int(wall) < self.floor:
            return {"status": "team_identity_clock_rollback", "floor": self.floor}
        generation = self.bootstrap.get("policyGeneration", 0)
        subject_leases = [
            lease
            for lease in self.leases
            if not lease.get("suppressed", False)
            and lease.get("policyGeneration") == generation
            and lease.get("subjectMember") == subject_member
        ]
        due_at = max(
            (lease.get("validUntil", 0) for lease in subject_leases), default=None
        )
        if due_at is None:
            return {"status": "rejected", "reason": "no_identity_lease"}
        grace_seconds = max(0, int(grace_seconds))
        grace_deadline = due_at + grace_seconds
        if int(wall) < due_at:
            renewal_state = "active"
        elif int(wall) < grace_deadline:
            renewal_state = "grace"
        else:
            renewal_state = "suspended"
        self.floor = max(self.floor, int(wall))
        renewal = {
            "subjectMember": subject_member,
            "state": renewal_state,
            "dueAt": due_at,
            "graceDeadline": grace_deadline,
            "verifierAvailable": bool(verifier_available),
            "backgroundIdpRequests": 0,
        }
        self.renewals[subject_member] = renewal
        self.audit_count += 1
        self.effect_count += 1
        self.persist()
        return {"status": renewal_state, "renewal": copy.deepcopy(renewal)}


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def adversarial_stimulus(name: str, base: str, state):
    """Mint an otherwise-valid live presentation with one targeted defect."""

    material = state.current_keys()
    issuer = state.issuer_url(base)
    now = int(state.clock.wall_time())
    normal_header = {"alg": "RS256", "kid": material.rsa_kid, "typ": "JWT"}
    normal_claims = {
        "iss": issuer,
        "aud": state.scenario.get("client_id", "ee-team-client"),
        "sub": "adversarial-user",
        "iat": now,
        "auth_time": now,
        "exp": now + 300,
        "jti": "adversarial-jti",
    }

    def encoded(value) -> bytes:
        return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()

    def sign_segments(header_segment: bytes, claims_segment: bytes) -> bytes:
        signing_input = header_segment + b"." + claims_segment
        signature = material.sign("RS256", signing_input)
        return signing_input + b"." + b64url(signature).encode("ascii")

    def signed_compact(header_bytes: bytes, claims_bytes: bytes) -> bytes:
        return sign_segments(
            b64url(header_bytes).encode("ascii"),
            b64url(claims_bytes).encode("ascii"),
        )

    valid_token = signed_compact(encoded(normal_header), encoded(normal_claims))
    discovery = {
        "issuer": issuer,
        "device_authorization_endpoint": f"{base}/device",
        "token_endpoint": f"{base}/token",
        "jwks_uri": f"{base}/jwks",
        "token_endpoint_auth_methods_supported": ["none"],
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
    }
    valid_jwk = material.rsa_public_jwk()

    if name == "duplicate_discovery":
        remainder = {key: value for key, value in discovery.items() if key != "issuer"}
        body = (
            b'{"issuer":'
            + encoded(issuer)
            + b',"issuer":'
            + encoded(issuer)
            + b","
            + encoded(remainder)[1:]
        )
        return "application/json", body
    if name == "duplicate_device":
        return "application/json", (
            b'{"device_code":"device","user_code":"USER-CODE",'
            + b'"verification_uri":'
            + encoded(f"{base}/activate")
            + b","
            b'"expires_in":900,"expires_in":900,"interval":5}'
        )
    if name == "duplicate_token":
        token_json = encoded(valid_token.decode("ascii"))
        return "application/json", (
            b'{"access_token":"access","token_type":"Bearer","id_token":'
            + token_json
            + b',"id_token":'
            + token_json
            + b"}"
        )
    if name == "duplicate_jwks":
        key_array = encoded([valid_jwk])
        return "application/json", b'{"keys":' + key_array + b',"keys":' + key_array + b"}"
    if name == "duplicate_jwk":
        remainder = {key: value for key, value in valid_jwk.items() if key != "kid"}
        raw_jwk = (
            b'{"kid":'
            + encoded(valid_jwk["kid"])
            + b',"kid":'
            + encoded(valid_jwk["kid"])
            + b","
            + encoded(remainder)[1:]
        )
        return "application/json", b'{"keys":[' + raw_jwk + b"]}"
    if name == "duplicate_jose_header":
        raw_header = (
            b'{"alg":"RS256","alg":"RS256","kid":'
            + encoded(material.rsa_kid)
            + b',"typ":"JWT"}'
        )
        return "text/plain", signed_compact(raw_header, encoded(normal_claims))
    if name == "duplicate_claims":
        remainder = {key: value for key, value in normal_claims.items() if key != "sub"}
        raw_claims = (
            b'{"sub":"adversarial-user","sub":"adversarial-user",'
            + encoded(remainder)[1:]
        )
        return "text/plain", signed_compact(encoded(normal_header), raw_claims)
    if name == "json_depth_65":
        nested = "[" * 65 + "0" + "]" * 65
        body = encoded(discovery)[:-1] + b',"extra":' + nested.encode() + b"}"
        return "application/json", body
    if name == "json_oversize":
        oversized = dict(discovery, padding="x" * (1024 * 1024 + 1))
        return "application/json", encoded(oversized)
    if name == "compact_two_segments":
        return "text/plain", valid_token.rsplit(b".", 1)[0]
    if name == "compact_four_segments":
        return "text/plain", valid_token + b".extra"
    if name == "compact_empty_segment":
        header_segment = b64url(encoded(normal_header)).encode("ascii")
        signature = material.sign("RS256", header_segment + b".")
        return "text/plain", header_segment + b".." + b64url(signature).encode("ascii")
    if name == "compact_whitespace":
        return "text/plain", b" " + valid_token + b"\n"
    if name in {"compact_padded", "compact_standard_base64"}:
        encoder = (
            base64.urlsafe_b64encode
            if name == "compact_padded"
            else base64.b64encode
        )
        header_segment = encoder(encoded(normal_header))
        claims_with_filler = dict(normal_claims, filler="\u083e")
        claims_segment = encoder(
            json.dumps(
                claims_with_filler,
                sort_keys=True,
                separators=(",", ":"),
                ensure_ascii=False,
            ).encode("utf-8")
        )
        signing_input = header_segment + b"." + claims_segment
        signature = material.sign("RS256", signing_input)
        return "text/plain", signing_input + b"." + encoder(signature)

    jose_headers = {
        "header_unknown_crit": {"crit": ["future"], "future": True},
        "header_jku": {"jku": "https://attacker.invalid/jwks"},
        "header_x5u": {"x5u": "https://attacker.invalid/cert"},
        "header_jwk": {"jwk": {"kty": "oct", "k": "AA"}},
        "header_x5c": {"x5c": ["AA=="]},
    }
    if name in jose_headers:
        header = dict(normal_header, **jose_headers[name])
        return "text/plain", signed_compact(encoded(header), encoded(normal_claims))
    if name == "header_missing_kid":
        header = {key: value for key, value in normal_header.items() if key != "kid"}
        return "text/plain", signed_compact(encoded(header), encoded(normal_claims))
    if name == "header_alg_none":
        header = dict(normal_header, alg="none")
        return "text/plain", (
            b64url(encoded(header)).encode("ascii")
            + b"."
            + b64url(encoded(normal_claims)).encode("ascii")
            + b"."
        )
    if name == "header_alg_confusion":
        header = dict(normal_header, alg="HS256")
        header_segment = b64url(encoded(header)).encode("ascii")
        claims_segment = b64url(encoded(normal_claims)).encode("ascii")
        signing_input = header_segment + b"." + claims_segment
        secret = base64.urlsafe_b64decode(
            valid_jwk["n"] + "=" * (-len(valid_jwk["n"]) % 4)
        )
        signature = hmac.new(secret, signing_input, hashlib.sha256).digest()
        return "text/plain", signing_input + b"." + b64url(signature).encode("ascii")

    discovery_urls = {
        "url_insecure_http": "http://127.0.0.1/device",
        "url_userinfo": "https://user:password@127.0.0.1/device",
        "url_fragment": f"{base}/device#fragment",
        "url_reserved": "https://192.0.2.1/device",
    }
    if name in discovery_urls:
        attacked = dict(
            discovery,
            device_authorization_endpoint=discovery_urls[name],
        )
        return "application/json", encoded(attacked)
    raise KeyError(name)


ADVERSARIAL_STIMULUS_NAMES = sorted(
    [
        "compact_empty_segment",
        "compact_four_segments",
        "compact_padded",
        "compact_standard_base64",
        "compact_two_segments",
        "compact_whitespace",
        "duplicate_claims",
        "duplicate_device",
        "duplicate_discovery",
        "duplicate_jose_header",
        "duplicate_jwk",
        "duplicate_jwks",
        "duplicate_token",
        "header_alg_confusion",
        "header_alg_none",
        "header_jku",
        "header_jwk",
        "header_missing_kid",
        "header_unknown_crit",
        "header_x5c",
        "header_x5u",
        "json_depth_65",
        "json_oversize",
        "url_fragment",
        "url_insecure_http",
        "url_reserved",
        "url_userinfo",
    ]
)


def run_openssl(args, data=None):
    proc = subprocess.run(
        ["openssl", *args], input=data, capture_output=True, check=True
    )
    return proc.stdout


class KeyMaterial:
    """One signing key generation: RSA-2048 (RS256) + P-256 (ES256)."""

    def __init__(self, state_dir: str, generation: int):
        self.generation = generation
        self.rsa_kid = f"rs{generation}"
        self.ec_kid = f"es{generation}"
        self.rsa_key = os.path.join(state_dir, f"rsa-{generation}.pem")
        self.ec_key = os.path.join(state_dir, f"ec-{generation}.pem")
        run_openssl(["genrsa", "-out", self.rsa_key, "2048"])
        run_openssl(
            ["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", self.ec_key]
        )

    def rsa_public_jwk(self):
        return self.rsa_jwk_for(self.rsa_key, self.rsa_kid)

    @staticmethod
    def rsa_jwk_for(rsa_key: str, kid: str):
        text = run_openssl(
            ["rsa", "-in", rsa_key, "-text", "-noout"]
        ).decode("ascii", "replace")
        hex_digits = []
        in_modulus = False
        exponent = 65537
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("modulus:") or stripped.startswith("Modulus:"):
                in_modulus = True
                continue
            if stripped.startswith("publicExponent:") or stripped.startswith(
                "Exponent:"
            ):
                in_modulus = False
                exponent = int(stripped.split()[1])
                continue
            if in_modulus:
                if all(c in "0123456789abcdefABCDEF:" for c in stripped) and stripped:
                    hex_digits.append(stripped.replace(":", ""))
                else:
                    in_modulus = False
        modulus = bytes.fromhex("".join(hex_digits)).lstrip(b"\x00")
        exp_bytes = exponent.to_bytes((exponent.bit_length() + 7) // 8, "big")
        return {
            "kty": "RSA",
            "kid": kid,
            "use": "sig",
            "alg": "RS256",
            "n": b64url(modulus),
            "e": b64url(exp_bytes),
        }

    def ec_public_jwk(self):
        return self.ec_jwk_for(self.ec_key, self.ec_kid, "P-256", 32)

    @staticmethod
    def ec_jwk_for(ec_key: str, kid: str, curve: str, coordinate_bytes: int):
        text = run_openssl(
            ["ec", "-in", ec_key, "-text", "-noout"]
        ).decode("ascii", "replace")
        hex_digits = []
        in_pub = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("pub:"):
                in_pub = True
                continue
            if in_pub:
                if all(c in "0123456789abcdefABCDEF:" for c in stripped) and stripped:
                    hex_digits.append(stripped.replace(":", ""))
                else:
                    in_pub = False
        point = bytes.fromhex("".join(hex_digits))
        if point[0] != 0x04 or len(point) != 1 + 2 * coordinate_bytes:
            raise RuntimeError("unexpected EC public point encoding")
        return {
            "kty": "EC",
            "kid": kid,
            "use": "sig",
            "alg": "ES256",
            "crv": curve,
            "x": b64url(point[1 : 1 + coordinate_bytes]),
            "y": b64url(point[1 + coordinate_bytes :]),
        }

    def sign(self, alg: str, signing_input: bytes) -> bytes:
        if alg == "RS256":
            return run_openssl(
                ["dgst", "-sha256", "-sign", self.rsa_key], data=signing_input
            )
        if alg == "ES256":
            der = run_openssl(
                ["dgst", "-sha256", "-sign", self.ec_key], data=signing_input
            )
            return der_ecdsa_to_raw(der)
        raise ValueError(f"unsupported alg {alg}")


def der_ecdsa_to_raw(der: bytes, width: int = 32) -> bytes:
    """Convert DER SEQUENCE{r INTEGER, s INTEGER} to fixed-width JOSE r||s."""
    if der[0] != 0x30:
        raise ValueError("not a DER sequence")
    idx = 2
    if der[1] & 0x80:
        idx = 2 + (der[1] & 0x7F)

    def read_int(offset):
        if der[offset] != 0x02:
            raise ValueError("expected DER integer")
        length = der[offset + 1]
        value = der[offset + 2 : offset + 2 + length]
        return value.lstrip(b"\x00"), offset + 2 + length

    r, idx = read_int(idx)
    s, _ = read_int(idx)
    return r.rjust(width, b"\x00") + s.rjust(width, b"\x00")


class IdpState:
    def __init__(self, scenario: dict, state_dir: str):
        self.lock = threading.Lock()
        self.scenario = scenario
        self.state_dir = state_dir
        self.issuer_path = scenario.get("issuer_path", "")
        self.flow = scenario.get("flow", {})
        self.keys = [KeyMaterial(state_dir, 1)]
        self.retired_kids = []
        self.jwks_profile = "normal"
        self.jwks_mode = "fresh"
        self.jwks_stimulus_keys = {}
        self.devices = {}
        self.token_polls = {}
        self.minted_jtis = []
        self.control_log = []
        self.request_trace = []
        self.clock = ScenarioClock(scenario.get("logical_clock", {}))
        self.poll_oracle = PollOracle()
        self.network_oracle = NetworkOracle()
        self.capability_profile = scenario.get("capability_profile", "absent")
        if self.capability_profile not in CAPABILITY_PROFILES:
            raise ValueError(f"unknown capability profile: {self.capability_profile}")
        self.identity = IdentityOracle(
            state_dir, int(scenario.get("identity_floor", self.clock.wall_time()))
        )
        self.artifact_path = os.path.join(state_dir, "identity-artifact.json")
        self.artifact = None
        self.artifact_error = None
        if os.path.exists(self.artifact_path):
            with open(self.artifact_path, "r", encoding="utf-8") as handle:
                self.artifact = json.load(handle)
        self.transient_frame = None
        self.transient_frame_deadline = None
        self.sequence = 0
        generation_path = os.path.join(state_dir, "process-generation")
        process_generation = 0
        if os.path.exists(generation_path):
            with open(generation_path, "r", encoding="ascii") as handle:
                process_generation = int(handle.read().strip() or "0")
        self.process_generation = process_generation + 1
        self.lifecycle_process = None
        self.lifecycle_descendant_pid = None
        self.partial_token_buffer = bytearray()
        with open(generation_path, "w", encoding="ascii") as handle:
            handle.write(f"{self.process_generation}\n")

    def issuer_url(self, base: str) -> str:
        path = str(self.issuer_path).strip()
        if not path or path == "/":
            return base
        return base + "/" + path.strip("/")

    def next_identifier(self, prefix: str, width: int = 16) -> str:
        self.sequence += 1
        seed = str(self.scenario.get("deterministic_seed", "fake-idp-t7.7"))
        digest = hashlib.sha256(
            f"{seed}:{self.process_generation}:{prefix}:{self.sequence}".encode("utf-8")
        ).hexdigest()[:width]
        return f"{prefix}-{self.process_generation}-{digest}"

    def record_request(
        self, method: str, path: str, client_host: str, authorization_present: bool
    ) -> None:
        with self.lock:
            self.request_trace.append(
                {
                    "sequence": len(self.request_trace) + 1,
                    "method": method,
                    "path": path,
                    "clientHost": client_host,
                    "authorizationPresent": bool(authorization_present),
                    "wall": int(self.clock.wall_time()),
                    "monotonic": int(self.clock.monotonic_time()),
                }
            )

    def capability_snapshot(self) -> dict:
        features = list(CAPABILITY_PROFILES[self.capability_profile])
        return {
            "profile": self.capability_profile,
            "receiverFeatures": features,
            "advertisesTeamManifest": "mesh.team.manifest.v1" in features,
            "dispatchesIdentityAttest": (
                "mesh.team.identity_attested.v1" in features
            ),
        }

    def feature_disposition(self, required_features) -> dict:
        if (
            not isinstance(required_features, list)
            or len(required_features) > 32
            or any(
                not isinstance(feature, str)
                or not feature
                or len(feature.encode("utf-8")) > 64
                or not feature.startswith("mesh.")
                for feature in required_features
            )
            or required_features != sorted(set(required_features))
        ):
            return {
                "disposition": "quarantine",
                "reason": "mesh_event_feature_contract_invalid",
                "detail": "required_features_not_canonical",
            }
        required = set(required_features)
        mandatory = {
            "mesh.team.manifest.v1",
            "mesh.team.identity_attested.v1",
        }
        missing = sorted(mandatory - required)
        if missing:
            return {
                "disposition": "quarantine",
                "reason": "mesh_event_feature_contract_invalid",
                "missingMandatoryFeatures": missing,
            }
        unknown = sorted(required - mandatory)
        if unknown:
            return {
                "disposition": "replayable_unsupported",
                "reason": "unknown_required_feature",
                "unknownFeatures": unknown,
            }
        if self.capability_profile == "absent":
            return {
                "disposition": "no_dispatch",
                "reason": "team_identity_capability_absent",
            }
        if self.capability_profile == "manifest_only":
            return {
                "disposition": "replayable_unsupported",
                "reason": "identity_attested_not_supported",
            }
        return {"disposition": "eligible", "reason": "supported"}

    def frame_snapshot(self) -> dict:
        self.expire_ceremony_if_due()
        if self.transient_frame is None:
            return {"present": False}
        body = json.dumps(
            self.transient_frame, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        return {
            "present": True,
            "bytes": len(body),
            "frame": copy.deepcopy(self.transient_frame),
        }

    def expire_ceremony_if_due(self) -> bool:
        if (
            self.transient_frame_deadline is not None
            and self.clock.monotonic_time() >= self.transient_frame_deadline
        ):
            self.devices = {}
            self.token_polls = {}
            self.transient_frame = None
            self.transient_frame_deadline = None
            self.poll_oracle.reset()
            return True
        return False

    def artifact_snapshot(self) -> dict:
        if self.artifact is None:
            return {"present": False, "lastProjectionError": self.artifact_error}
        with open(self.artifact_path, "rb") as handle:
            file_bytes = handle.read()
        return {
            "present": True,
            "sha256": hashlib.sha256(file_bytes).hexdigest(),
            "bytes": len(file_bytes),
            "artifact": copy.deepcopy(self.artifact),
            "lastProjectionError": self.artifact_error,
        }

    def artifact_views_snapshot(self) -> dict:
        if self.artifact is None:
            return {"present": False, "views": {}}
        evidence = copy.deepcopy(self.artifact)
        digest = hashlib.sha256(
            json.dumps(evidence, sort_keys=True, separators=(",", ":")).encode(
                "utf-8"
            )
        ).hexdigest()
        return {
            "present": True,
            "views": {
                "database": {"identityEvidence": copy.deepcopy(evidence)},
                "manifest": {"identityEvidence": copy.deepcopy(evidence)},
                "audit": {"identityEvidence": copy.deepcopy(evidence)},
                "log": {"terminalStatus": "verified", "evidenceHash": digest},
                "supportBundle": {
                    "identityEvidence": copy.deepcopy(evidence),
                    "evidenceHash": digest,
                },
            },
        }

    def project_identity_artifact(
        self,
        token: str,
        claims: dict,
        header: dict,
        entry: dict,
        access_token: str,
        refresh_token,
        base: str,
    ) -> bool:
        del entry, access_token, refresh_token
        policy = self.scenario.get("privacy_policy", {})
        configured_allowed_groups = policy.get("allowed_groups", [])
        raw_max_matches = policy.get("max_allowed_group_matches", 8)
        invalid_policy = (
            not isinstance(configured_allowed_groups, list)
            or len(configured_allowed_groups) > 256
            or any(
                not isinstance(value, str)
                or not 0 < len(value.encode("utf-8")) <= 256
                for value in configured_allowed_groups
            )
            or isinstance(raw_max_matches, bool)
            or not isinstance(raw_max_matches, int)
            or not 0 <= raw_max_matches <= 256
        )
        if invalid_policy:
            self.artifact_error = "identity_policy_out_of_bounds"
            return False
        allowed_groups = list(configured_allowed_groups)
        max_matches = raw_max_matches
        source_groups = claims.get("groups", [])
        subject = claims.get("sub")
        email = claims.get("email")
        issuer = claims.get("iss")
        audience = claims.get("aud")
        issued_at = claims.get("iat")
        auth_time = claims.get("auth_time")
        expires_at = claims.get("exp")
        replay_claim = claims.get("jti")
        now = int(self.clock.wall_time())
        expected_alg = self.scenario.get("alg", "RS256")
        material = self.current_keys()
        expected_kid = (
            material.rsa_kid if expected_alg == "RS256" else material.ec_kid
        )
        defects = self.scenario.get("defects", {})
        has_defect = isinstance(defects, dict) and any(
            bool(value) for value in defects.values()
        )
        preview_email_unverified = (
            policy.get("preview_email", False)
            and claims.get("email_verified") is not True
        )
        scalar_claims_invalid = (
            not isinstance(issuer, str)
            or not 0 < len(issuer.encode("utf-8")) <= 2048
            or issuer != self.issuer_url(base)
            or not isinstance(audience, str)
            or not 0 < len(audience.encode("utf-8")) <= 512
            or audience != self.scenario.get("client_id", "ee-team-client")
            or any(
                isinstance(value, bool) or not isinstance(value, int)
                for value in (issued_at, auth_time, expires_at)
            )
            or expires_at <= now
            or issued_at > now + 600
            or auth_time > now + 600
            or not isinstance(replay_claim, str)
            or not 0 < len(replay_claim.encode("utf-8")) <= 512
            or header.get("alg") != expected_alg
            or header.get("kid") != expected_kid
            or has_defect
        )
        malformed_claims = (
            not isinstance(subject, str)
            or not 0 < len(subject.encode("utf-8")) <= 512
            or not isinstance(source_groups, list)
            or len(source_groups) > 256
            or any(
                not isinstance(value, str)
                or not 0 < len(value.encode("utf-8")) <= 256
                for value in source_groups
            )
            or (
                policy.get("preview_email", False)
                and (
                    not isinstance(email, str)
                    or not 0 < len(email.encode("utf-8")) <= 320
                    or preview_email_unverified
                )
            )
            or scalar_claims_invalid
        )
        if malformed_claims:
            self.artifact_error = (
                "identity_token_not_verified"
                if scalar_claims_invalid or preview_email_unverified
                else "identity_claims_out_of_bounds"
            )
            return False
        matches = sorted(set(source_groups).intersection(allowed_groups))
        bounded_matches = matches[:max_matches]
        jwk = (
            material.rsa_public_jwk()
            if header.get("alg") == "RS256"
            else material.ec_public_jwk()
        )
        thumbprint_members = (
            {"e": jwk["e"], "kty": jwk["kty"], "n": jwk["n"]}
            if jwk["kty"] == "RSA"
            else {
                "crv": jwk["crv"],
                "kty": jwk["kty"],
                "x": jwk["x"],
                "y": jwk["y"],
            }
        )
        artifact = {
            "schema": "fake_idp.identity_evidence.v1",
            "subject": subject,
            "groupDecision": {
                "allowed": bool(bounded_matches),
                "matchedAllowedGroups": bounded_matches,
                "matchLimit": max_matches,
                "truncated": len(matches) > len(bounded_matches),
            },
            "provenance": {
                "issuer": issuer,
                "clientId": audience,
                "tokenHash": hashlib.sha256(token.encode("ascii")).hexdigest(),
                "replayClaimHash": hashlib.sha256(
                    replay_claim.encode("utf-8")
                ).hexdigest(),
                "kid": header.get("kid"),
                "jwkThumbprint": b64url(
                    hashlib.sha256(
                        json.dumps(
                            thumbprint_members,
                            sort_keys=True,
                            separators=(",", ":"),
                        ).encode("utf-8")
                    ).digest()
                ),
                "alg": header.get("alg"),
                "verifiedAt": int(self.clock.wall_time()),
                "expiresAt": expires_at,
            },
        }
        if policy.get("preview_email", False):
            artifact["previewEmail"] = email
        self.artifact = artifact
        self.artifact_error = None
        atomic_write_json(self.artifact_path, artifact)
        return True

    def identity_digest(self) -> str:
        with open(self.identity.path, "rb") as handle:
            return hashlib.sha256(handle.read()).hexdigest()

    def start_lifecycle_trap(self) -> dict:
        if self.lifecycle_process is not None and self.lifecycle_process.poll() is None:
            return {"status": "already_running"}
        child_code = (
            "import subprocess,sys,time;"
            "child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],"
            "stdout=sys.stdout,stderr=sys.stderr);"
            "print(f'trap-ready:{child.pid}',flush=True);time.sleep(60)"
        )
        self.partial_token_buffer = bytearray(b"PARTIAL_TOKEN_SENTINEL_T77")
        self.lifecycle_process = subprocess.Popen(
            [sys.executable, "-c", child_code],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        assert self.lifecycle_process.stdout is not None
        readable, _, _ = select.select([self.lifecycle_process.stdout], [], [], 2.0)
        if not readable:
            self.cancel_lifecycle_trap()
            return {"status": "readiness_timeout"}
        ready_line = self.lifecycle_process.stdout.readline().decode(
            "ascii", "replace"
        ).strip()
        if not ready_line.startswith("trap-ready:"):
            self.cancel_lifecycle_trap()
            return {"status": "readiness_failed", "line": ready_line}
        self.lifecycle_descendant_pid = int(ready_line.split(":", 1)[1])
        return {
            "status": "running",
            "pid": self.lifecycle_process.pid,
            "descendantPid": self.lifecycle_descendant_pid,
            "partialTokenBytes": len(self.partial_token_buffer),
        }

    def cancel_lifecycle_trap(self) -> dict:
        process = self.lifecycle_process
        descendant_pid = self.lifecycle_descendant_pid
        if process is None:
            return {
                "status": "not_running",
                "reaped": True,
                "descendantsReaped": True,
                "partialTokenZeroized": not self.partial_token_buffer,
                "descendantObserved": descendant_pid is not None,
            }
        process_group = None
        if process.poll() is None:
            try:
                process_group = os.getpgid(process.pid)
            except ProcessLookupError:
                process_group = None
        if process_group is not None:
            try:
                os.killpg(process_group, signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            if process_group is not None:
                try:
                    os.killpg(process_group, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.wait(timeout=2)
        for index in range(len(self.partial_token_buffer)):
            self.partial_token_buffer[index] = 0
        self.partial_token_buffer.clear()
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()
        descendants_reaped = True
        if process_group is not None:
            for _ in range(100):
                try:
                    os.killpg(process_group, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.01)
            else:
                descendants_reaped = False
        self.lifecycle_process = None
        self.lifecycle_descendant_pid = None
        self.devices = {}
        self.token_polls = {}
        self.transient_frame = None
        self.transient_frame_deadline = None
        self.poll_oracle.reset()
        return {
            "status": "cancelled",
            "reaped": process.poll() is not None,
            "descendantsReaped": descendants_reaped,
            "descendantObserved": descendant_pid is not None,
            "partialTokenZeroized": not self.partial_token_buffer,
            "freshCeremonyRequired": True,
        }

    def current_keys(self):
        return self.keys[-1]

    def _jwks_stimulus_key(self, profile: str) -> dict:
        cached = self.jwks_stimulus_keys.get(profile)
        if cached is not None:
            return cached
        if profile in {"rsa_1024", "rsa_bad_exponent", "ambiguous_same_kid"}:
            bits = 1024 if profile == "rsa_1024" else 2048
            exponent = 3 if profile == "rsa_bad_exponent" else 65537
            key_path = os.path.join(self.state_dir, f"jwks-{profile}.pem")
            run_openssl(
                [
                    "genpkey",
                    "-algorithm",
                    "RSA",
                    "-out",
                    key_path,
                    "-pkeyopt",
                    f"rsa_keygen_bits:{bits}",
                    "-pkeyopt",
                    f"rsa_keygen_pubexp:{exponent}",
                ]
            )
            kid = "ambiguous-rs" if profile == "ambiguous_same_kid" else profile
            cached = {
                "keyPath": key_path,
                "jwk": KeyMaterial.rsa_jwk_for(key_path, kid),
                "alg": "RS256",
                "rawWidth": None,
            }
        elif profile == "ec_wrong_curve":
            key_path = os.path.join(self.state_dir, "jwks-ec-p384.pem")
            run_openssl(
                [
                    "ecparam",
                    "-name",
                    "secp384r1",
                    "-genkey",
                    "-noout",
                    "-out",
                    key_path,
                ]
            )
            cached = {
                "keyPath": key_path,
                "jwk": KeyMaterial.ec_jwk_for(
                    key_path, "wrong-curve-es256", "P-384", 48
                ),
                "alg": "ES256",
                "rawWidth": 48,
            }
        else:
            raise ValueError(f"profile has no stimulus key: {profile}")
        self.jwks_stimulus_keys[profile] = cached
        return cached

    def token_signing_identity(self, requested_alg: str) -> tuple[str, str]:
        if self.jwks_profile in {"rsa_1024", "rsa_bad_exponent"}:
            profile = self._jwks_stimulus_key(self.jwks_profile)
            return profile["alg"], profile["jwk"]["kid"]
        if self.jwks_profile == "ec_wrong_curve":
            profile = self._jwks_stimulus_key(self.jwks_profile)
            return profile["alg"], profile["jwk"]["kid"]
        if self.jwks_profile == "ambiguous_same_kid":
            self._jwks_stimulus_key("ambiguous_same_kid")
            return "RS256", "ambiguous-rs"
        material = self.current_keys()
        kid = material.rsa_kid if requested_alg == "RS256" else material.ec_kid
        return requested_alg, kid

    def sign_presentation(self, signing_input: bytes, alg: str) -> bytes:
        if self.jwks_profile in {
            "rsa_1024",
            "rsa_bad_exponent",
            "ec_wrong_curve",
        }:
            profile = self._jwks_stimulus_key(self.jwks_profile)
            signature = run_openssl(
                ["dgst", "-sha256", "-sign", profile["keyPath"]],
                data=signing_input,
            )
            if alg == "ES256":
                return der_ecdsa_to_raw(signature, width=profile["rawWidth"])
            return signature
        return self.current_keys().sign(alg, signing_input)

    def rotate_keys(self, retire_previous: bool):
        with self.lock:
            if retire_previous:
                previous = self.keys[-1]
                self.retired_kids.extend([previous.rsa_kid, previous.ec_kid])
            self.keys.append(KeyMaterial(self.state_dir, len(self.keys) + 1))

    def jwks(self):
        with self.lock:
            keys = []
            for material in self.keys:
                if material.rsa_kid not in self.retired_kids:
                    keys.append(material.rsa_public_jwk())
                if material.ec_kid not in self.retired_kids:
                    keys.append(material.ec_public_jwk())
            profile = self.jwks_profile
            if profile == "rsa_1024":
                keys = [copy.deepcopy(self._jwks_stimulus_key(profile)["jwk"])]
            elif profile == "rsa_bad_exponent":
                keys = [copy.deepcopy(self._jwks_stimulus_key(profile)["jwk"])]
            elif profile == "ec_wrong_curve":
                keys = [copy.deepcopy(self._jwks_stimulus_key(profile)["jwk"])]
            elif profile == "missing_kid":
                for key in keys:
                    key.pop("kid", None)
            elif profile == "duplicate_same_kid":
                keys = [keys[0], copy.deepcopy(keys[0])]
            elif profile == "ambiguous_same_kid":
                first = self.current_keys().rsa_public_jwk()
                first["kid"] = "ambiguous-rs"
                second = copy.deepcopy(self._jwks_stimulus_key(profile)["jwk"])
                keys = [first, second]
            elif profile == "metadata_mismatch":
                for key in keys:
                    key["use"] = "enc"
                    key["key_ops"] = ["encrypt"]
                    key["alg"] = "HS256"
            elif profile == "zero_eligible":
                for key in keys:
                    key.pop("use", None)
                    key["key_ops"] = ["deriveKey"]
            return {"keys": keys}


def make_handler(state: IdpState, base_url_holder: dict):
    class Handler(BaseHTTPRequestHandler):
        server_version = "FakeIdp/1"

        def log_message(self, *_args):
            pass

        def _send_bytes(
            self, status: int, body: bytes, content_type: str, extra_headers=None
        ):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            for key, value in (extra_headers or {}).items():
                self.send_header(key, value)
            self.end_headers()
            if body:
                self.wfile.write(body)

        def _send_json(self, status: int, payload: dict, extra_headers=None):
            body = json.dumps(
                payload, sort_keys=True, separators=(",", ":")
            ).encode("utf-8")
            self._send_bytes(status, body, "application/json", extra_headers)

        def _read_body(self) -> bytes:
            length = int(self.headers.get("Content-Length", "0"))
            return self.rfile.read(min(length, 65536))

        def _handle_stall_trap(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "2")
            self.end_headers()
            time.sleep(1)
            try:
                self.wfile.write(b"{}")
            except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                pass

        def _handle_partial_token_trap(self):
            prefix = b'{"access_token":"PARTIAL_TOKEN_SENTINEL_T77'
            suffix = b'","token_type":"Bearer"}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(prefix) + len(suffix)))
            self.end_headers()
            try:
                self.wfile.write(prefix)
                self.wfile.flush()
                time.sleep(1)
                self.wfile.write(suffix)
            except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                pass

        def do_GET(self):
            state.record_request(
                "GET",
                self.path,
                self.client_address[0],
                self.headers.get("Authorization") is not None,
            )
            base = base_url_holder["base"]
            path = self.path.split("?", 1)[0]
            if path == "/.well-known/openid-configuration":
                issuer = state.issuer_url(base)
                self._send_json(
                    200,
                    {
                        "issuer": issuer,
                        "device_authorization_endpoint": f"{base}/device",
                        "token_endpoint": f"{base}/token",
                        "jwks_uri": f"{base}/jwks",
                        "grant_types_supported": [
                            "urn:ietf:params:oauth:grant-type:device_code"
                        ],
                        "token_endpoint_auth_methods_supported": (
                            [
                                state.scenario.get(
                                    "token_auth_method", "client_secret_post"
                                )
                            ]
                            if state.scenario.get("secret_required")
                            else ["none"]
                        ),
                        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
                        "claims_supported": [
                            "aud",
                            "auth_time",
                            "email",
                            "email_verified",
                            "exp",
                            "groups",
                            "iat",
                            "iss",
                            "jti",
                            "sub",
                        ],
                        "scopes_supported": ["openid", "email", "profile", "groups"],
                    },
                )
            elif path == "/jwks":
                jwks = state.jwks()
                etag = '"' + hashlib.sha256(
                    json.dumps(jwks, sort_keys=True, separators=(",", ":")).encode(
                        "utf-8"
                    )
                ).hexdigest() + '"'
                if self.headers.get("If-None-Match") == etag or (
                    state.jwks_mode == "stale_304"
                    and self.headers.get("If-None-Match") is not None
                ):
                    self._send_bytes(304, b"", "application/json", {"ETag": etag})
                else:
                    self._send_json(
                        200,
                        jwks,
                        {"Cache-Control": "max-age=60", "ETag": etag},
                    )
            elif path == "/_capabilities":
                with state.lock:
                    payload = state.capability_snapshot()
                self._send_json(200, payload)
            elif path == "/_artifact":
                with state.lock:
                    payload = state.artifact_snapshot()
                self._send_json(200, payload)
            elif path == "/_artifact_views":
                with state.lock:
                    payload = state.artifact_views_snapshot()
                self._send_json(200, payload)
            elif path == "/_frame":
                with state.lock:
                    payload = state.frame_snapshot()
                self._send_json(200, payload)
            elif path == "/_state":
                with state.lock:
                    state.expire_ceremony_if_due()
                    payload = {
                        # Private harness introspection. This is intentionally
                        # secret-bearing and is never an artifact projection.
                        "devices": {
                            code: dict(entry, minted_jtis=None)
                            for code, entry in state.devices.items()
                        },
                        "retired_kids": list(state.retired_kids),
                        "jwksProfile": state.jwks_profile,
                        "jwksMode": state.jwks_mode,
                        "generations": len(state.keys),
                        "processGeneration": state.process_generation,
                        "minted_jtis": list(state.minted_jtis),
                        "control_log": list(state.control_log),
                        "requestTrace": copy.deepcopy(state.request_trace),
                        "clock": state.clock.snapshot(),
                        "pollOracle": state.poll_oracle.snapshot(),
                        "identityOracle": state.identity.snapshot(
                            int(state.clock.wall_time())
                        ),
                        "identityOracleSha256": state.identity_digest(),
                        "capabilities": state.capability_snapshot(),
                        "frame": state.frame_snapshot(),
                        "artifact": state.artifact_snapshot(),
                        "lifecycleTrap": {
                            "running": state.lifecycle_process is not None
                            and state.lifecycle_process.poll() is None,
                            "descendantPid": state.lifecycle_descendant_pid,
                            "partialTokenBytes": len(state.partial_token_buffer),
                        },
                    }
                self._send_json(200, payload)
            elif path == "/_stimulus":
                self._send_json(200, {"names": ADVERSARIAL_STIMULUS_NAMES})
            elif path.startswith("/_stimulus/"):
                name = path.removeprefix("/_stimulus/")
                try:
                    content_type, body = adversarial_stimulus(name, base, state)
                except KeyError:
                    self._send_json(404, {"error": "unknown_stimulus"})
                    return
                self._send_bytes(200, body, content_type)
            elif path == "/_trap/redirect/same_origin":
                self._send_bytes(
                    302,
                    b"",
                    "text/plain",
                    {"Location": f"{base}/jwks"},
                )
            elif path == "/_trap/redirect/cross_origin":
                self._send_bytes(
                    302,
                    b"",
                    "text/plain",
                    {"Location": "https://127.0.0.1:1/credential-sink"},
                )
            elif path == "/_trap/oversize":
                self._send_bytes(
                    200,
                    b"x" * (1024 * 1024 + 1),
                    "application/octet-stream",
                )
            elif path == "/_trap/stall":
                self._handle_stall_trap()
            elif path == "/_trap/partial_token":
                self._handle_partial_token_trap()
            elif path == "/_trap/redaction_diagnostic":
                self._send_json(
                    500,
                    {
                        "error": "provider_failure",
                        "diagnostic": "ACCESS_TOKEN_SENTINEL_T77",
                    },
                )
            else:
                self._send_json(404, {"error": "not_found"})

        def do_POST(self):
            state.record_request(
                "POST",
                self.path,
                self.client_address[0],
                self.headers.get("Authorization") is not None,
            )
            path = self.path.split("?", 1)[0]
            if path == "/device":
                self._handle_device()
            elif path == "/token":
                self._handle_token()
            elif path == "/_trap/stall":
                self._read_body()
                self._handle_stall_trap()
            elif path == "/_trap/partial_token":
                self._read_body()
                self._handle_partial_token_trap()
            elif path == "/_control":
                self._handle_control()
            else:
                self._send_json(404, {"error": "not_found"})

        def _handle_device(self):
            self._read_body()
            flow = state.flow
            with state.lock:
                device_code = state.next_identifier("dev")
                user_code_raw = state.next_identifier("user", 8).rsplit("-", 1)[-1]
                user_code = f"{user_code_raw[:4]}-{user_code_raw[4:8]}".upper()
                ceremony_id = state.next_identifier("ceremony")
                interval = flow.get("interval", 5)
                state.devices[device_code] = {
                    "status": flow.get("initial_status", "authorization_pending"),
                    "user_code": user_code,
                    "ceremony_id": ceremony_id,
                    "issued_at": state.clock.wall_time(),
                    "issued_monotonic": state.clock.monotonic_time(),
                    "polls": 0,
                    "interval": interval,
                }
            base = base_url_holder["base"]
            payload = {
                "device_code": device_code,
                "user_code": user_code,
                "verification_uri": f"{base}/activate",
                "verification_uri_complete": f"{base}/activate?user_code={user_code}",
            }
            if "expires_in" in flow:
                payload["expires_in"] = flow["expires_in"]
            else:
                payload["expires_in"] = 900
            if "interval" in flow:
                payload["interval"] = flow["interval"]
            for key in flow.get("device_response_omit", []):
                payload.pop(key, None)
            with state.lock:
                state.transient_frame = {
                    "schema": "mesh.team.identity_attest.v1",
                    "ceremonyId": ceremony_id,
                    "ttlSeconds": int(flow.get("frame_ttl", 300)),
                    "verificationUrl": payload.get("verification_uri_complete"),
                    "userCode": payload.get("user_code"),
                    "status": "identity_pending",
                }
                frame_ttl = max(0, int(flow.get("frame_ttl", 300)))
                state.transient_frame_deadline = (
                    state.clock.monotonic_time() + frame_ttl
                )
            self._send_json(200, payload)

        def _handle_token(self):
            body = self._read_body().decode("utf-8", "replace")
            params = {}
            for pair in body.split("&"):
                if "=" in pair:
                    key, _, value = pair.partition("=")
                    params[key] = value
            device_code = params.get("device_code", "")
            if state.scenario.get("secret_required"):
                method = state.scenario.get(
                    "token_auth_method", "client_secret_post"
                )
                if method == "client_secret_post":
                    authenticated = bool(params.get("client_secret"))
                else:
                    authorization = self.headers.get("Authorization", "")
                    authenticated = authorization.startswith("Basic ") and bool(
                        authorization.removeprefix("Basic ").strip()
                    )
                if not authenticated:
                    self._send_json(401, {"error": "invalid_client"})
                    return
            response_status = 500
            response_payload = {"error": "unhandled_status"}
            with state.lock:
                entry = state.devices.get(device_code)
                if entry is None:
                    response_status = 400
                    response_payload = {"error": "invalid_grant"}
                else:
                    entry["polls"] += 1
                    flow = state.flow
                    script = flow.get("poll_script", [])
                    if entry["polls"] <= len(script):
                        entry["status"] = script[entry["polls"] - 1]
                    expires_in = flow.get("expires_in", 900)
                    if (
                        isinstance(expires_in, int)
                        and not isinstance(expires_in, bool)
                        and expires_in >= 0
                        and state.clock.monotonic_time()
                        - entry["issued_monotonic"]
                        >= expires_in
                    ):
                        entry["status"] = "expired_token"
                    status = entry["status"]
                    if status == "authorization_pending":
                        slow_after = flow.get("slow_down_after_polls")
                        if slow_after is not None and entry["polls"] > slow_after:
                            entry["interval"] += 5
                            response_payload = {"error": "slow_down"}
                        else:
                            response_payload = {"error": "authorization_pending"}
                        response_status = 400
                    elif status == "slow_down":
                        entry["interval"] += 5
                        response_status = 400
                        response_payload = {"error": "slow_down"}
                    elif status == "access_denied":
                        response_status = 400
                        response_payload = {"error": "access_denied"}
                    elif status == "expired_token":
                        response_status = 400
                        response_payload = {"error": "expired_token"}
                    elif status == "granted":
                        token, claims, header = self._mint_id_token(entry)
                        response_config = state.scenario.get("token_response", {})
                        access_token = response_config.get("access_token")
                        if access_token is None:
                            access_token = state.next_identifier("access")
                        refresh_token = response_config.get("refresh_token")
                        response_status = 200
                        response_payload = {
                            "access_token": access_token,
                            "token_type": "Bearer",
                            "id_token": token,
                        }
                        if refresh_token is not None:
                            response_payload["refresh_token"] = refresh_token
                        project_verified = bool(
                            state.scenario.get("project_verified_artifact", False)
                        )
                        projection_succeeded = False
                        if project_verified:
                            projection_succeeded = state.project_identity_artifact(
                                token,
                                claims,
                                header,
                                entry,
                                access_token,
                                refresh_token,
                                base_url_holder["base"],
                            )
                        if state.transient_frame is not None:
                            state.transient_frame["status"] = (
                                "verified"
                                if projection_succeeded
                                else (
                                    "token_rejected"
                                    if project_verified
                                    else "token_received"
                                )
                            )
            self._send_json(response_status, response_payload)

        def _mint_id_token(self, entry: dict):
            del entry
            claims_config = state.scenario.get("claims", {})
            requested_alg = state.scenario.get("alg", "RS256")
            alg, kid = state.token_signing_identity(requested_alg)
            now = int(state.clock.wall_time())
            jti = state.next_identifier("jti")
            payload = {
                "iss": claims_config.get(
                    "iss", state.issuer_url(base_url_holder["base"])
                ),
                "aud": claims_config.get("aud", "ee-team-client"),
                "sub": claims_config.get("sub", "user-priya"),
                "email": claims_config.get("email", "priya@example.test"),
                "email_verified": claims_config.get("email_verified", True),
                "iat": now,
                "auth_time": now,
                "exp": now + claims_config.get("lifetime_seconds", 300),
                "jti": jti,
            }
            if "groups" in claims_config:
                payload["groups"] = claims_config["groups"]
            payload.update(claims_config.get("extra", {}))
            for key in claims_config.get("omit", []):
                payload.pop(key, None)

            # Token-defect injection for the JOSE attack matrix. Each defect
            # produces a token that a correct verifier (T7.5) MUST reject; the
            # harness's job is to be able to MINT each one, deterministically.
            defects = state.scenario.get("defects", {})
            header = {"alg": alg, "typ": "JWT", "kid": kid}
            if defects.get("wrong_kid"):
                header["kid"] = "kid-not-in-jwks"
            if defects.get("alg_none"):
                # Unsigned "none" token: no signature segment content.
                header["alg"] = "none"
                unsigned = (
                    b64url(json.dumps(header, separators=(",", ":")).encode())
                    + "."
                    + b64url(json.dumps(payload, separators=(",", ":")).encode())
                    + "."
                )
                state.minted_jtis.append(jti)
                return unsigned, payload, header
            if "header_alg" in defects:
                # Algorithm-confusion: advertise one alg, sign with another.
                header["alg"] = defects["header_alg"]

            if defects.get("noncanonical_base64url"):
                # Sign the exact transmitted standard-base64 + padded segments.
                # The only defect is JOSE's strict unpadded-base64url contract.
                header_segment = base64.b64encode(
                    json.dumps(header, separators=(",", ":")).encode()
                )
                payload_segment = base64.b64encode(
                    json.dumps(payload, separators=(",", ":")).encode()
                )
                signing_input = header_segment + b"." + payload_segment
                signature = state.sign_presentation(signing_input, alg)
                token = (
                    signing_input
                    + b"."
                    + base64.b64encode(signature)
                ).decode("ascii")
                state.minted_jtis.append(jti)
                return token, payload, header

            signing_input = (
                b64url(json.dumps(header, separators=(",", ":")).encode())
                + "."
                + b64url(json.dumps(payload, separators=(",", ":")).encode())
            ).encode("ascii")
            signature = state.sign_presentation(signing_input, alg)
            token = signing_input.decode("ascii") + "." + b64url(signature)
            if defects.get("bad_signature"):
                # Flip the final signature byte so verification must fail.
                head, _, sig = token.rpartition(".")
                raw = bytearray(
                    base64.urlsafe_b64decode(sig + "=" * (-len(sig) % 4))
                )
                raw[-1] ^= 0x01
                token = head + "." + b64url(bytes(raw))
            state.minted_jtis.append(jti)
            return token, payload, header

        def _handle_control(self):
            try:
                command = json.loads(self._read_body().decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                self._send_json(400, {"error": "bad_control_payload"})
                return
            action = command.get("action")
            with state.lock:
                state.control_log.append(action or "unknown")
            if action == "set_status":
                target_status = command.get("status", "granted")
                user_code = command.get("user_code")
                with state.lock:
                    changed = 0
                    for entry in state.devices.values():
                        if user_code is None or entry["user_code"] == user_code:
                            entry["status"] = target_status
                            changed += 1
                self._send_json(200, {"ok": True, "changed": changed})
            elif action == "rotate_keys":
                state.rotate_keys(bool(command.get("retire_previous", False)))
                self._send_json(200, {"ok": True, "generations": len(state.keys)})
            elif action == "set_jwks_profile":
                profile = command.get("profile", "normal")
                mode = command.get("mode", "fresh")
                if profile not in JWKS_PROFILES or mode not in {"fresh", "stale_304"}:
                    self._send_json(400, {"error": "unsupported_jwks_profile"})
                    return
                with state.lock:
                    state.jwks_profile = profile
                    state.jwks_mode = mode
                self._send_json(200, {"ok": True})
            elif action == "set_flow":
                with state.lock:
                    state.flow.update(command.get("flow", {}))
                self._send_json(200, {"ok": True})
            elif action == "replace_flow":
                flow = command.get("flow", {})
                if not isinstance(flow, dict):
                    self._send_json(400, {"error": "flow_must_be_object"})
                    return
                with state.lock:
                    state.flow = copy.deepcopy(flow)
                    state.scenario["flow"] = state.flow
                self._send_json(200, {"ok": True})
            elif action == "set_privacy_policy":
                policy = command.get("privacy_policy", {})
                if not isinstance(policy, dict):
                    self._send_json(400, {"error": "privacy_policy_must_be_object"})
                    return
                with state.lock:
                    state.scenario["privacy_policy"] = copy.deepcopy(policy)
                self._send_json(200, {"ok": True})
            elif action == "set_claims":
                claims = command.get("claims", {})
                if not isinstance(claims, dict):
                    self._send_json(400, {"error": "claims_must_be_object"})
                    return
                with state.lock:
                    state.scenario.setdefault("claims", {}).update(
                        copy.deepcopy(claims)
                    )
                self._send_json(200, {"ok": True})
            elif action == "set_secret_required":
                method = command.get(
                    "method",
                    state.scenario.get("token_auth_method", "client_secret_post"),
                )
                if method not in {"client_secret_basic", "client_secret_post"}:
                    self._send_json(400, {"error": "unsupported_token_auth_method"})
                    return
                with state.lock:
                    state.scenario["secret_required"] = bool(
                        command.get("required", False)
                    )
                    state.scenario["token_auth_method"] = method
                self._send_json(200, {"ok": True})
            elif action == "set_alg":
                alg = command.get("alg")
                if alg not in {"RS256", "ES256"}:
                    self._send_json(400, {"error": "unsupported_alg"})
                    return
                with state.lock:
                    state.scenario["alg"] = alg
                self._send_json(200, {"ok": True})
            elif action == "set_capability_profile":
                profile = command.get("profile")
                if profile not in CAPABILITY_PROFILES:
                    self._send_json(400, {"error": "unknown_capability_profile"})
                    return
                with state.lock:
                    state.capability_profile = profile
                    result = state.capability_snapshot()
                self._send_json(200, {"ok": True, "result": result})
            elif action == "feature_disposition":
                with state.lock:
                    result = state.feature_disposition(
                        command.get("required_features", [])
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "clock_set":
                try:
                    with state.lock:
                        state.clock.set(command.get("wall"), command.get("monotonic"))
                        result = state.clock.snapshot()
                except (TypeError, ValueError) as error:
                    self._send_json(400, {"error": str(error)})
                    return
                self._send_json(200, {"ok": True, "result": result})
            elif action == "clock_advance":
                try:
                    with state.lock:
                        state.clock.advance(
                            command.get("wall", 0), command.get("monotonic", 0)
                        )
                        result = state.clock.snapshot()
                except (TypeError, ValueError) as error:
                    self._send_json(400, {"error": str(error)})
                    return
                self._send_json(200, {"ok": True, "result": result})
            elif action == "poll_reset":
                with state.lock:
                    state.poll_oracle.reset()
                    result = state.poll_oracle.snapshot()
                self._send_json(200, {"ok": True, "result": result})
            elif action == "network_evaluate":
                with state.lock:
                    result = state.network_oracle.evaluate(command.get("spec", {}))
                self._send_json(200, {"ok": True, "result": result})
            elif action == "output_budget_evaluate":
                stdout_bytes = command.get("stdout_bytes")
                stderr_bytes = command.get("stderr_bytes")
                if (
                    isinstance(stdout_bytes, bool)
                    or not isinstance(stdout_bytes, int)
                    or isinstance(stderr_bytes, bool)
                    or not isinstance(stderr_bytes, int)
                    or stdout_bytes < 0
                    or stderr_bytes < 0
                    or stdout_bytes > MAX_U64
                    or stderr_bytes > MAX_U64
                    or stdout_bytes > MAX_U64 - stderr_bytes
                ):
                    result = {
                        "status": "terminated",
                        "reason": "output_size_invalid_or_overflow",
                        "reapRequired": True,
                    }
                else:
                    aggregate = stdout_bytes + stderr_bytes
                    result = {
                        "status": "allowed" if aggregate <= 65536 else "terminated",
                        "reason": (
                            "within_output_cap"
                            if aggregate <= 65536
                            else "aggregate_output_cap_exceeded"
                        ),
                        "aggregateBytes": aggregate,
                        "reapRequired": aggregate > 65536,
                    }
                self._send_json(200, {"ok": True, "result": result})
            elif action == "poll_configure":
                with state.lock:
                    start = int(
                        command.get("start", state.clock.monotonic_time())
                    )
                    result = state.poll_oracle.configure(
                        command.get("response", {}), start
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "poll_attempt":
                if "now" in command:
                    self._send_json(
                        400,
                        {"error": "poll_attempt_uses_logical_monotonic_clock"},
                    )
                    return
                with state.lock:
                    now = int(state.clock.monotonic_time())
                    result = state.poll_oracle.attempt(
                        command.get("event", "authorization_pending"), now
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "poll_repeat_legal":
                with state.lock:
                    result = state.poll_oracle.repeat_legal(
                        command.get("event", "authorization_pending"),
                        int(command.get("count", 1)),
                        state.clock,
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "identity_reset":
                with state.lock:
                    state.identity.reset(int(command.get("floor", 0)))
                    result = state.identity.snapshot(int(state.clock.wall_time()))
                self._send_json(200, {"ok": True, "result": result})
            elif action == "identity_path":
                with state.lock:
                    result = state.identity.evaluate_path(
                        command.get("path", ""),
                        int(command.get("wall", state.clock.wall_time())),
                    )
                    result["identityOracleSha256"] = state.identity_digest()
                self._send_json(200, {"ok": True, "result": result})
            elif action == "observe_time_evidence":
                with state.lock:
                    before = state.identity.snapshot(int(state.clock.wall_time()))
                    result = {
                        "status": "ignored_for_floor",
                        "peerProducedAt": command.get("peer_produced_at"),
                        "tokenTimestamp": command.get("token_timestamp"),
                        "attestationTimestamp": command.get(
                            "attestation_timestamp"
                        ),
                        "receiptTime": command.get("receipt_time"),
                        "floor": before["floor"],
                        "identityOracleSha256": state.identity_digest(),
                    }
                self._send_json(200, {"ok": True, "result": result})
            elif action == "identity_admit_lease":
                with state.lock:
                    result = state.identity.admit_lease(
                        command.get("lease", {}),
                        int(command.get("wall", state.clock.wall_time())),
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "identity_repair":
                with state.lock:
                    result = state.identity.repair_floor(
                        int(command.get("new_floor", state.identity.floor)),
                        bool(command.get("confirmed", False)),
                        bool(command.get("corrected_clock", False)),
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "replay_claim":
                with state.lock:
                    wall = int(command.get("wall", state.clock.wall_time()))
                    result = state.identity.replay_claim(
                        str(command.get("issuer", "https://issuer.example")),
                        str(command.get("client_id", "ee-team-client")),
                        str(command.get("claim", "")),
                        str(command.get("claimant", "unknown")),
                        int(command.get("expires_at", wall + 300)),
                        wall,
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "bootstrap_enable":
                with state.lock:
                    result = state.identity.bootstrap_enable(
                        int(command.get("generation", 1)),
                        int(command.get("grace_seconds", 0)),
                        int(command.get("wall", state.clock.wall_time())),
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "bootstrap_verify":
                with state.lock:
                    result = state.identity.bootstrap_verify(
                        str(command.get("subject_member", "")),
                        str(command.get("verifier_member", "")),
                        str(command.get("subject_issuer", "")),
                        str(command.get("subject", "")),
                        str(command.get("verifier_issuer", "")),
                        str(command.get("verifier_subject", "")),
                        int(command.get("lease_seconds", 300)),
                        int(command.get("wall", state.clock.wall_time())),
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "bootstrap_tick":
                with state.lock:
                    result = state.identity.bootstrap_tick(
                        int(command.get("wall", state.clock.wall_time()))
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "renewal_tick":
                with state.lock:
                    result = state.identity.renewal_tick(
                        str(command.get("subject_member", "")),
                        int(command.get("wall", state.clock.wall_time())),
                        int(command.get("grace_seconds", 0)),
                        bool(command.get("verifier_available", False)),
                    )
                self._send_json(200, {"ok": True, "result": result})
            elif action == "purge_ceremony":
                with state.lock:
                    state.devices = {}
                    state.token_polls = {}
                    state.transient_frame = None
                    state.transient_frame_deadline = None
                    state.poll_oracle.reset()
                self._send_json(
                    200,
                    {
                        "ok": True,
                        "result": {
                            "outerState": state.identity.outer_state,
                            "freshCeremonyRequired": True,
                        },
                    },
                )
            elif action == "start_lifecycle_trap":
                with state.lock:
                    result = state.start_lifecycle_trap()
                self._send_json(200, {"ok": True, "result": result})
            elif action == "cancel_lifecycle_trap":
                with state.lock:
                    result = state.cancel_lifecycle_trap()
                self._send_json(200, {"ok": True, "result": result})
            else:
                self._send_json(400, {"error": "unknown_action"})

    return Handler


def build_tls(state_dir: str) -> ssl.SSLContext:
    ca_key = os.path.join(state_dir, "ca-key.pem")
    ca_pem = os.path.join(state_dir, "ca.pem")
    ca_config = os.path.join(state_dir, "ca.cnf")
    srv_key = os.path.join(state_dir, "server-key.pem")
    srv_csr = os.path.join(state_dir, "server.csr")
    srv_pem = os.path.join(state_dir, "server.pem")
    ext = os.path.join(state_dir, "san.cnf")
    run_openssl(["genrsa", "-out", ca_key, "2048"])
    with open(ca_config, "w", encoding="ascii") as handle:
        handle.write(
            "[req]\n"
            "distinguished_name=dn\n"
            "x509_extensions=v3_ca\n"
            "prompt=no\n"
            "[dn]\n"
            "CN=fake-idp-ephemeral-ca\n"
            "[v3_ca]\n"
            "basicConstraints=critical,CA:true\n"
            "keyUsage=critical,keyCertSign,cRLSign\n"
            "subjectKeyIdentifier=hash\n"
        )
    run_openssl(
        [
            "req", "-x509", "-new", "-key", ca_key, "-sha256", "-days", "2",
            "-config", ca_config, "-out", ca_pem,
        ]
    )
    with open(ext, "w", encoding="ascii") as handle:
        handle.write(
            "basicConstraints=critical,CA:false\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            "extendedKeyUsage=serverAuth\n"
            "subjectAltName=IP:127.0.0.1,DNS:localhost\n"
        )
    run_openssl(["genrsa", "-out", srv_key, "2048"])
    run_openssl(
        ["req", "-new", "-key", srv_key, "-subj", "/CN=127.0.0.1", "-out", srv_csr]
    )
    run_openssl(
        [
            "x509", "-req", "-in", srv_csr, "-CA", ca_pem, "-CAkey", ca_key,
            "-CAcreateserial", "-days", "2", "-sha256", "-extfile", ext,
            "-out", srv_pem,
        ]
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(srv_pem, srv_key)
    return context


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", required=True)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--scenario", default=None)
    args = parser.parse_args()

    os.makedirs(args.dir, exist_ok=True)
    scenario = {}
    if args.scenario:
        with open(args.scenario, "r", encoding="utf-8") as handle:
            scenario = json.load(handle)

    state = IdpState(scenario, args.dir)
    base_url_holder = {"base": ""}
    handler = make_handler(state, base_url_holder)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    server.daemon_threads = True
    server.socket = build_tls(args.dir).wrap_socket(server.socket, server_side=True)
    port = server.socket.getsockname()[1]
    base_url_holder["base"] = f"https://127.0.0.1:{port}"

    ready_path = os.path.join(args.dir, "ready")
    with open(ready_path + ".tmp", "w", encoding="ascii") as handle:
        handle.write(f"{port} {os.getpid()} {state.process_generation}\n")
    os.replace(ready_path + ".tmp", ready_path)

    def terminate(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, terminate)

    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        state.cancel_lifecycle_trap()
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
