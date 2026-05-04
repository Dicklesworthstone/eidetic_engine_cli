# Skill Capsule Review Template

```yaml
schema: ee.skill.procedure_distillation.skill_capsule_review.v1
capsuleId: "<capsule id>"
procedureId: "<procedure id>"
sourceExportId: "<export id>"
artifactPath: "<path>"
artifactHash: "<blake3 hash>"
installMode: render_only
sourceEvidence:
  sourceRunIds: ["<run id>"]
  evidenceIds: ["<evidence id>"]
  verificationIds: ["<verification id>"]
reviewChecklist:
  frontmatterValid: false
  triggerDescriptionNamesUseWhen: false
  requiredSectionsPresent: false
  outputTemplatePresent: false
  noAutomaticInstallationLanguage: true
  redactionStatusRecorded: false
  degradedBehaviorRecorded: false
  unsupportedClaimsRecorded: false
  verificationEvidencePresent: false
reviewDecision:
  disposition: refuse | needs_review | ready_for_manual_copy
  reason: "<brief evidence-linked reason>"
firstFailureDiagnosis: "<diagnosis or null>"
```

Review rules:

- Render-only means no automatic installation, no file copying, and no mutation
  of a live skills directory.
- A capsule can be ready for manual copy only after verification evidence is
  present and the review checklist passes.
- The review may quote the capsule frontmatter and section names, but not raw
  private evidence.
