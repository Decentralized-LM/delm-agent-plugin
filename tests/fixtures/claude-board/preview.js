// Deterministic UI fixture. Every command is immediate and sends no model prompt.
import {renderBoard, renderCompact} from './board-render.js';
import {cases} from './states.js';

let state = {snapshot: cases.working, screen: {kind: 'overview'}};
let visible = false;

function actions($) {
  const update = screen => { state.screen = screen; $.ui.invalidate('ui.render'); };
  return {
    details: () => update({kind: 'details'}), back: () => update({kind: 'overview'}),
    select: (kind, id) => update({kind, id}), page: () => {},
    hide: async () => { visible = false; await $.ui.close({id: 'delm'}); $.ui.invalidate('ui.render'); },
    show: async () => { visible = true; await $.ui.open({id: 'delm', title: 'DeLM', rows: 24, columns: 54}); $.ui.invalidate('ui.render'); },
  };
}

export function register(on) {
  on('session.start', async ($, e, next) => {
    for (const name of ['board-preview', 'board-details', 'board-hide', 'board-complete', 'board-stopped', 'board-preparing', 'board-unicode', 'board-attention']) {
      await $.command.register({name, description: 'Local DeLM UI fixture with sample data', immediate: true});
    }
    return next(e);
  });
  for (const name of ['board-preview', 'board-details', 'board-hide', 'board-complete', 'board-stopped', 'board-preparing', 'board-unicode', 'board-attention']) {
    on('command.run', {command: name}, async $ => {
      const nextCase = cases[name.replace('board-', '')] ? name.replace('board-', '') : 'working';
      state = {snapshot: cases[nextCase], screen: {kind: name === 'board-details' || ['complete', 'stopped', 'attention'].includes(nextCase) ? 'details' : 'overview'}};
      if (name === 'board-hide') await actions($).hide();
      else await actions($).show();
      return {text: 'Local preview · sample data · no model call.'};
    });
  }
  on('ui.close', {id: 'delm'}, ($, e, next) => { visible = false; $.ui.invalidate('ui.render'); return next(e); });
  on('ui.render', {component: 'Pane'}, ($, e, next) => e.requestId === 'delm' ? renderBoard($.ui.resolve(e), e, state, actions($)) : next(e));
  on('ui.render', {component: 'AbovePrompt'}, ($, e, next) => {
    const result = renderCompact($.ui.resolve(e), e, {...state, paneShown: visible}, actions($));
    return result || next(e);
  });
}
