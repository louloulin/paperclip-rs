import { test, expect } from '@playwright/test';

// R872 spec 06 — skill lifecycle.
//
// Verifies:
// 1. Create skill via POST /api/skills with body, name, scope (company|agent)
// 2. Skill appears in agent's effective skill list (resolver)
// 3. Edit skill → version increments (immutable history)
// 4. Delete skill → resolver no longer returns it
// 5. Skill source-of-truth mirrored to filesystem (skills_home) — R870 sync

test.describe('skill lifecycle', () => {
  test('create → list → edit → delete', async ({ request }) => {
    test.skip(true, 'awaiting R865 auth + R872 skills route wiring');

    // 1. Create
    const create = await request.post('/api/skills', {
      data: {
        name: 'summarize-doc',
        body: 'When asked, summarize the document into 3 bullets.',
        scope: 'agent',
        scope_id: 'agent-1',
      },
    });
    expect(create.status()).toBe(201);
    const skill = await create.json();

    // 2. List
    const list = await request.get('/api/skills?scope=agent&scope_id=agent-1');
    expect(list.status()).toBe(200);
    const skills = await list.json();
    expect(skills.find((s: { id: string }) => s.id === skill.id)).toBeTruthy();

    // 3. Edit (creates new version)
    const edit = await request.put(`/api/skills/${skill.id}`, {
      data: { body: 'Summarize into 5 bullets max.' },
    });
    expect(edit.status()).toBe(200);
    const edited = await edit.json();
    expect(edited.version).toBeGreaterThan(skill.version);

    // 4. Delete
    const del = await request.delete(`/api/skills/${skill.id}`);
    expect(del.status()).toBe(204);

    // 5. Verify gone
    const after = await request.get('/api/skills?scope=agent&scope_id=agent-1');
    const remaining = await after.json();
    expect(remaining.find((s: { id: string }) => s.id === skill.id)).toBeUndefined();
  });

  test('filesystem mirror (skills_home sync)', async ({ request }) => {
    test.skip(true, 'awaiting R870 skills_home sync wiring');
  });

  test('immutable history: old versions still queryable', async ({ request }) => {
    test.skip(true, 'awaiting versioned skill table');
  });
});