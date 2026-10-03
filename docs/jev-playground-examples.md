# Jev Playground examples

These examples exercise Jev's three decision types with realistic, ambiguous, and evidence-sensitive cases. Each one has a state object and a matching question map.

## Using the examples

For each case, paste the state JSON into the Playground's **State** editor and the question JSON into **Questions**, then run it. The question map contains one question ID and its definition. You can add more entries to that map to compare multiple questions against the same state.

- **Noul** estimates how likely the condition in the instructions is to be true. Its `criteria` explain what counts as true and false.
- **Choice** picks among the descriptions in its `criteria` object. Include a fallback such as `none` or `unclear` when appropriate.
- **Score** estimates a position on its ordered `criteria` array, from the lowest level to the highest.

Jev returns structured decisions and probabilities, not a written rationale. Treat these as probes for exploring behavior, not as a substitute for validating consequential decisions.

## Noul — estimate whether a statement is true

### 1. A convincing but suspicious payment email

Tests whether Jev weighs the actual link destination and sender domain over the email's branding and tone.

**State**

```json
{
  "message": {
    "from": "PayPal Account Review <security@paypaI-notice.example>",
    "subject": "Unusual activity — review required today",
    "body": "We noticed a sign-in from a new device. Your account will be limited in 12 hours unless you review the activity.",
    "visible_link_text": "Review recent activity",
    "link_url": "https://paypal.com.secure-check.example/login"
  },
  "recipient_context": "The recipient has a PayPal account but did not initiate a support request."
}
```

**Question**

```json
{
  "q_phishing": {
    "type": "noul",
    "instructions": "Should the recipient treat this message as a phishing attempt?",
    "criteria": {
      "true": "The sender or link impersonates a trusted service, or uses urgency to obtain account access.",
      "false": "The sender and destination are verified official channels, and the message is expected."
    }
  }
}
```

### 2. A release with one important unknown

Tests whether completed checks outweigh a missing rollback safeguard.

**State**

```json
{
  "release": "2.8.0",
  "checks": {
    "unit_tests": "passed",
    "staging_smoke_test": "passed",
    "migration_review": "approved",
    "production_backup": "not recorded",
    "rollback_steps": "not documented"
  },
  "plan": "Apply the database migration and deploy to all production users at once."
}
```

**Question**

```json
{
  "q_release_ready": {
    "type": "noul",
    "instructions": "Is the release ready to proceed to production now?",
    "criteria": {
      "true": "Required checks are complete and there is a verified recovery or rollback path.",
      "false": "A critical safeguard is missing, unknown, or has not been verified."
    }
  }
}
```

### 3. A tentative delivery date

Tests the difference between a possible date and a firm commitment.

**State**

```json
{
  "project": "Mobile checkout redesign",
  "meeting_notes": [
    "Mira: We could probably ship Thursday if QA is clean.",
    "Jon: The payment retry test is still flaky.",
    "Mira: Let's confirm the date after the next test run."
  ],
  "calendar_entry": "Tentative launch window: Thursday"
}
```

**Question**

```json
{
  "q_firm_commitment": {
    "type": "noul",
    "instructions": "Has the team made a firm commitment to ship on Thursday?",
    "criteria": {
      "true": "The team explicitly confirmed Thursday as the committed ship date.",
      "false": "Thursday is conditional, tentative, or still awaiting confirmation."
    }
  }
}
```

## Choice — select the best option

### 4. Route a damaged-order request

Tests whether Jev distinguishes the customer's desired outcome from the best first action.

**State**

```json
{
  "ticket": "My glass set arrived with one cracked glass. I would rather get a replacement than a refund.",
  "order": {
    "delivered_days_ago": 2,
    "items": 4,
    "photo_attached": false
  },
  "policy": "For shipping damage, request a photo before sending a replacement. Do not make the customer repeat information already in the ticket."
}
```

**Question**

```json
{
  "q_damage_next_step": {
    "type": "choice",
    "instructions": "What should the support agent do first?",
    "criteria": {
      "request_photo": "Acknowledge the damage and ask for a photo so the replacement can be arranged.",
      "send_replacement": "Send a replacement immediately without further evidence.",
      "issue_refund": "Refund the order even though the customer asked for a replacement.",
      "escalate": "Escalate to a supervisor because the agent cannot handle the request.",
      "unclear": "The available information does not support any of the listed actions."
    }
  }
}
```

### 5. Respond to an ambiguous deletion request

Tests whether Jev notices scope and reversibility before recommending an irreversible action.

**State**

```json
{
  "conversation": [
    {"speaker": "Customer", "text": "Can you clear this workspace out? We are done with the old project."},
    {"speaker": "Agent", "text": "Do you mean remove the old files, archive the workspace, or delete the entire workspace?"},
    {"speaker": "Customer", "text": "Just get rid of the old stuff."
    }
  ],
  "workspace": {
    "members": 4,
    "last_backup": "unknown",
    "deletion_reversible": false
  }
}
```

**Question**

```json
{
  "q_workspace_response": {
    "type": "choice",
    "instructions": "What is the safest useful next response?",
    "criteria": {
      "clarify_scope": "Ask which specific files or resources should be removed and confirm the intended scope.",
      "explain_consequences": "Explain that full workspace deletion is irreversible and ask for explicit confirmation before proceeding.",
      "delete_workspace": "Delete the entire workspace based on the customer's ambiguous wording.",
      "archive_workspace": "Archive the workspace without confirming that this is what the customer wants.",
      "escalate": "Escalate because the request cannot be safely interpreted by the agent."
    }
  }
}
```

### 6. Pick an incident response

Tests decision-making with a strong temporal signal but incomplete root-cause evidence.

**State**

```json
{
  "service": "checkout-api",
  "observations": {
    "http_5xx_rate": "12%, up from 0.4%",
    "started": "4 minutes after version 5.14.2 deployed",
    "checkout_success_rate": "down 18%",
    "data_integrity_alerts": 0,
    "rollback_available": true,
    "cause_confirmed": false
  },
  "on_call_note": "The new deployment is the leading suspect, but no trace has confirmed the failing component."
}
```

**Question**

```json
{
  "q_incident_action": {
    "type": "choice",
    "instructions": "Which action should the on-call engineer take first?",
    "criteria": {
      "rollback": "Roll back the recent deployment while monitoring recovery.",
      "investigate_live": "Keep the affected version running and investigate before mitigating.",
      "scale_up": "Add capacity without evidence that resource saturation is the cause.",
      "declare_security": "Treat the incident as a security breach despite no supporting signal.",
      "status_update": "Notify stakeholders, but take no immediate mitigation action."
    }
  }
}
```

## Score — place the state on an ordered scale

### 7. Prioritize a production regression

Tests whether Jev considers reach, severity, and rollback availability together.

**State**

```json
{
  "change": "Search indexing optimization",
  "impact": {
    "search_queries_returning_empty": "31% (baseline 2%)",
    "affected_users": "all users on the new index",
    "started_after_deploy": true,
    "rollback_available": true,
    "data_loss_confirmed": false
  },
  "workaround": "Users can retry with a shorter query, but results remain incomplete."
}
```

**Question**

```json
{
  "q_regression_severity": {
    "type": "score",
    "instructions": "How severe is this production regression for incident triage?",
    "criteria": [
      "No user-visible impact; normal monitoring is sufficient.",
      "Minor inconvenience affecting a small number of users; a simple workaround exists.",
      "Material degradation affecting a subset of users or a core workflow with a usable workaround.",
      "Broad failure of an important workflow with no practical workaround; immediate mitigation is needed.",
      "Critical outage, data loss, or safety/security impact requiring the highest-priority response."
    ]
  }
}
```

### 8. Rate the strength of a causal claim

Tests whether Jev separates correlation from evidence that supports causation.

**State**

```json
{
  "claim": "The new onboarding email caused the increase in paid conversions.",
  "evidence": {
    "conversion_rate_before": "4.8% last week",
    "conversion_rate_after": "5.3% this week",
    "email_change_date": "Monday",
    "other_changes": [
      "A seasonal promotion also started Monday.",
      "The traffic source mix shifted toward returning customers."
    ],
    "experiment": "No randomized holdout was used."
  }
}
```

**Question**

```json
{
  "q_causal_evidence": {
    "type": "score",
    "instructions": "How strongly does the evidence support the claim that the email caused the conversion increase?",
    "criteria": [
      "No relevant evidence supports the causal claim.",
      "A weak association is observed, with major alternative explanations.",
      "Some evidence supports the claim, but important confounders remain.",
      "Strong evidence supports the claim, with alternatives mostly addressed.",
      "Causal evidence is highly convincing, such as a well-run randomized test with consistent results."
    ]
  }
}
```

### 9. Judge whether a feature is ready to roll out

Tests a readiness judgment where quality signals conflict with a known accessibility gap.

**State**

```json
{
  "feature": "Keyboard shortcuts for the editor",
  "pilot": {
    "participants": 42,
    "task_completion": "91%",
    "crash_free_sessions": "99.7%",
    "support_tickets": 3,
    "known_issue": "The shortcut guide is not usable with a screen reader.",
    "mitigation": "A text-only accessible guide is implemented but has not been tested with screen-reader users."
  },
  "proposal": "Enable the feature for every account tomorrow."
}
```

**Question**

```json
{
  "q_rollout_readiness": {
    "type": "score",
    "instructions": "How ready is this feature for a full rollout tomorrow?",
    "criteria": [
      "Not ready: major quality or access blockers remain.",
      "Early: core behavior works, but important fixes or validation are still needed.",
      "Promising: most goals are met; a limited rollout and targeted checks are appropriate.",
      "Nearly ready: remaining issues are minor, understood, and have verified mitigations.",
      "Ready: key quality, accessibility, and support checks are complete for a broad rollout."
    ]
  }
}
```
