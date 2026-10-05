export const working = {
  schema_version: 1, type: 'view', run_id: 'local-preview', session_id: 'preview-session', revision: 1,
  source: {controller_sequence: 12, board_sequence: 13, journal_offset: 0}, observed_at: 1791144000000,
  status: 'running', finished: false, freshness: {unavailable: []}, outcome: {},
  agents: [
    {slot: 1, native_state: 'working', task_ids: ['3']},
    {slot: 2, native_state: 'working', task_ids: ['2']},
  ],
  tasks: {items: [
    {id: '2', task_number: 2, title: 'Mapping preview', owner: 'worker-2', state: 'claimed', version: 10, dependencies: []},
    {id: '3', task_number: 3, title: 'Import endpoint', owner: 'worker-1', state: 'claimed', version: 12, dependencies: [5]},
    {id: '4', task_number: 4, title: 'Error summary', owner: null, state: 'available', version: 4, dependencies: []},
    {id: '1', task_number: 1, title: 'CSV validation', owner: 'worker-1', state: 'done', version: 9, dependencies: []},
  ], total: 4, offset: 0, limit: 24, next_offset: null},
  shared: {items: [
    {id: '5', kind: 'publication', worker: 'worker-1', revision: 1, title: 'CSV result shape',
      text: 'Rows contain validated contacts. Errors identify their source row.', files: ['src/import/schema.ts'], imported_by: ['worker-2']},
  ], total: 1, offset: 0, limit: 24, next_offset: null},
  checks: {items: [{id: '7', worker: 'worker-1', revision: 1, summary: 'CSV validation cases', passed: true,
    inputs_unchanged: true, reusable: true, scope: 'src/import/schema.ts'}], total: 1, offset: 0, limit: 24, next_offset: null},
};

export const preparing = {...working, status: 'preparing', agents: [], tasks: {items: [], total: 0}, shared: {items: [], total: 0}, checks: {items: [], total: 0}};
export const complete = {...working, status: 'delivered', finished: true,
  outcome: {delivered: true, verification_required: true, cleanup_complete: true, recovery_saved: false},
  agents: working.agents.map(agent => ({...agent, native_state: 'stopped'}))};
export const stopped = {...complete, status: 'stopped', outcome: {delivered: false, verification_required: false,
  cleanup_complete: true, recovery_saved: true, recovery_path: '/Users/example/Library/Application Support/DeLM/runs/local-preview/workspace/recovery'}};

export const unicode = {...working,
  tasks: {...working.tasks, items: working.tasks.items.map((task, index) => ({...task,
    title: index === 0 ? '日本語の取り込みを検証 — e\u0301 and 👩‍💻 — preserve every field in an unusually long title'
      : index === 1 ? 'Validate family emoji 👨‍👩‍👧‍👦 and flags 🇯🇵 without splitting a terminal cell' : task.title}))},
  shared: {...working.shared, items: [{...working.shared.items[0], title: '共有データの契約 · a very long shared contribution title with combining e\u0301 and 👩‍💻'}]},
};
export const attention = {...complete, status: 'delivery_conflict', attention: 'A project file changed while the agents were working.',
  outcome: {delivered: false, verification_required: false, cleanup_complete: false,
    conflicts: ['src/import/contract.ts'], conflicts_total: 1,
    retained_workspace_path: '/Users/example/Library/Application Support/DeLM/runs/local-preview/workspace',
    reason: 'Delivery could not replace the changed file. The completed agent work remains available.'}};

export const recovery = {...working, status: 'recovery_required',
  attention: 'Native shutdown confirmation is pending. Workspaces are preserved.',
  agents: working.agents.map(agent => ({...agent, native_state: 'stopped'}))};

export const cases = {working, preparing, complete, stopped, unicode, attention, recovery};
