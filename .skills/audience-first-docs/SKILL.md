---
name: audience-first-docs
description: Write, rewrite, or review documentation so it creates understanding quickly for a narrowly defined audience. Use when Codex is asked to improve docs, READMEs, guides, onboarding material, product docs, API docs, tutorials, explainers, or documentation style; especially when docs should be succinct, procedural, audience-specific, example-driven, or should rule out likely misunderstandings.
---

# Audience-First Docs

## Goal

Create documentation that answers the reader's next question before they have to ask it. Prefer procedural explanations, recognizable examples, and the smallest amount of context needed for the target audience to act correctly.

## Workflow

1. Define the audience narrowly before writing.
   - Bad: "developers"
   - Better: "backend engineers exposing a local web service to teammates"
   - Best: "backend engineers who already run local dev servers and need stable HTTPS URLs without fixed-port conflicts"
2. Write down the reader's likely questions in order.
   - "What problem is this solving?"
   - "Is this for my situation?"
   - "What do I type first?"
   - "What changes in my existing workflow?"
   - "What happens behind the scenes?"
   - "What should I not assume?"
3. Answer those questions in that order.
   - Start with the reader's existing pain or task.
   - Introduce only the concepts needed for the next action.
   - Use one concrete example before generalizing.
   - Explain mechanisms procedurally: "first this runs, then this is discovered, then this route is created."
4. Remove generic material.
   - Delete paragraphs that would apply to any product.
   - Delete examples the audience would not recognize from their own work.
   - Replace abstract benefits with concrete before/after behavior.
5. Add misunderstanding guards.
   - Contrast similar concepts early.
   - State boundaries directly: "Local tunnels stay on your machine; cloud tunnels appear in the dashboard."
   - Name prerequisites before commands that depend on them.
   - Mention the hidden mechanism when it explains surprising behavior.
   - Distinguish command syntax from the underlying mechanism. For example, write "listen on port 0, which tells the operating system to pick a free port" in prose, and reserve `PORT=0` for command examples or environment-variable references.

## Writing Rules

- Always state the target audience, as narrowly as possible, in notes or in the document itself when appropriate.
- Prefer procedure over taxonomy. People understand "do A, then B happens" faster than category lists.
- Put the first successful path before references, options, or edge cases.
- Use examples that match the audience's existing tools, errors, file names, and commands.
- Keep sentences short. One idea per paragraph.
- Use "this means..." sparingly to translate unfamiliar mechanisms into reader outcomes.
- Explain enough internals to prevent wrong mental models, not to show implementation detail.
- Do not let shorthand imply the wrong interface. If a value appears in a command as an environment variable, flag, or config key but represents a platform mechanism, name the mechanism first and the syntax second.
- Make contrasts explicit when names are similar.
- Keep reference tables after the workflow, not before it.
- Prefer stable, copyable commands over prose.

## Structure Template

Use this shape unless the artifact has a stronger existing structure:

1. Audience: one narrow sentence.
2. Problem: one recognizable situation or error.
3. Core model: the smallest explanation that makes the tool make sense.
4. First procedure: copyable steps to get a successful result.
5. What just happened: procedural explanation of the mechanism.
6. Common variations: only the variations this audience likely needs.
7. Misunderstandings: short bullets that rule out wrong assumptions.
8. Reference: commands, flags, variables, or API surface.

## Quality Check

Before finalizing, verify:

- The audience is narrow enough that another audience would need different docs.
- The opening answers "why should I care?" in the reader's terms.
- The first example is immediately recognizable to that audience.
- Each section answers a likely question at the moment it would arise.
- Procedural explanations outnumber abstract descriptions.
- Similar concepts are contrasted before the reader can confuse them.
- The document is shorter after the rewrite unless missing information was added deliberately.
