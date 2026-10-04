// Exercises the production observer and native board with saved local fixtures.
// Commands are immediate: this module never submits a prompt or launches agents.
import {registerBoard, observeBoard} from './board-view.js';

export function register(on) {
  registerBoard(on);
  on('session.start', async ($, e, next) => {
    await $.command.register({name: 'delm-status', description: 'Show the DeLM board', immediate: true});
    await $.command.register({name: 'board-hide', description: 'Hide the local sample board', immediate: true});
    return next(e);
  });
  on('command.run', {command: 'delm:run'}, async $ => {
    const session = await $.session.id();
    const result = await $.process.run(['/usr/bin/python3', $.plugin.root + '/bin/seed.py', '--seed',
      $.plugin.root + '/../home', session], {stdin: '', timeoutMs: 3000});
    if (result.exitCode !== 0) return {text: 'Local fixture could not be prepared.', exitCode: 1};
    const ready = JSON.parse(result.stdout);
    const run = {session, ready: {run_id: ready.run_id}, revision: 1, finished: false,
      agents: {'native-agent-1': {slot: 1, status: 'running', turn: 'turn-1', deliveredRevision: 1},
        'native-agent-2': {slot: 2, status: 'running', turn: 'turn-2', deliveredRevision: 1}}};
    observeBoard(run);
    await $.store.set('native-run:' + session, run);
    return {text: 'Local sample run · real read-only observer · no model call.'};
  });
  on('command.run', {command: 'board-hide'}, async $ => {
    await $.ui.close({id: 'delm'});
    return {text: 'Board closed. The sample run remains available.'};
  });
}
