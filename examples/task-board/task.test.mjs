import assert from 'node:assert/strict';
import test from 'node:test';
import model from './task.js';

test('new tasks require a title and start incomplete', () => {
  assert.deepEqual(model.createTask('one', '  Write notes  '), { id: 'one', title: 'Write notes', complete: false });
  assert.throws(() => model.createTask('two', ' '));
});

test('toggling and removing do not mutate the original list', () => {
  const tasks = [model.createTask('one', 'Write notes'), model.createTask('two', 'Review notes')];
  const updated = model.toggleTask(tasks, 'one');
  assert.equal(updated[0].complete, true);
  assert.equal(tasks[0].complete, false);
  assert.deepEqual(model.removeTask(updated, 'one'), [tasks[1]]);
});
