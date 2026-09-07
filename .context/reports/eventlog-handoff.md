# Eventlog infrastructure handoff

The user requires reusable setup and maintenance across arbitrary repositories
and Drovefiles. Infrastructure belongs in ../event-log and its skill; Drove
should consume it, without separately engineered reactor machinery.

## Current product capability

- eventlog init creates the log, vocabulary/config templates and Git metadata.
- eventlog doctor --fix installs guards and the skill.
- eventlog react owns locks, resume, intent, authorization, Git observations,
  outcome normalization and acknowledgments. It invokes a supplied action.
- Reactor actions and Drove wiring in the event-log repository are local
  examples, not a reusable installation/upgrade feature.
- No upstream product changes were made in this attempt.

## Remaining work

1. Define and implement reusable setup/maintenance upstream: repository
   declarations/settings, previewable changes, repeatable application and
   upgrades that preserve customization; update the distributed skill.
2. Put reusable action/process handling and its tests upstream. Review found
   timeout descendant/stdout risks, staged-index isolation issues, dirty-file
   accounting gaps, lifecycle claim renewal, and doc-loop coverage gaps.
3. Validate identical setup on a fresh repository and Drove.
4. Apply Drove migration and recover its missing acknowledgments/documentation
   from the evidence in eventlog-cutover.md before restarting its reactors.

## Archived work

Archive: `/Users/jjmartin/Development/Drove-archives/eventlog-drafts-20260907T142525Z`

The three worker workspaces are closed and their worktrees unregistered.
Their baseline-only local branches were removed; archive metadata records their names.
Full snapshots, patches, base SHAs, status lists and terminal transcripts are
preserved there. Drafts are not accepted or ready for production.
The existing migration branches point to main's v0.1.2 baseline; no draft
commits or PR were created. Drove product files on main remain unchanged.

Drove's legacy reactors remain stopped intentionally; the coordinator,
maintenance and files workspaces are retained. No reactor was restarted, no
checkpoint advanced, and no event log or reactor lock was deleted.
