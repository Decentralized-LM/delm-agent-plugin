import {test, expect} from 'claude-code/testing';

test('task and contribution buttons navigate through the native handler chain', async $ => {
  const pane = await $.ui.mount({plugin: 'delm-board-preview', surface: 'terminal', component: 'Pane', requestId: 'delm',
    props: {title: 'DeLM', isFocused: true, bodyColumns: 52, placement: 'dock', scroll: {offset: 0, bodyRows: 35}, view: {}}});
  expect(await pane.find({key: 'task-3'})).toBeDefined();
  await pane.press({key: 'task-3'});
  expect(await pane.find({type: 'Text', text: 'Task detail'})).toBeDefined();
  await pane.press({key: 'board-back'});
  expect(await pane.find({key: 'shared-publication-1'})).toBeDefined();
  await pane.press({key: 'shared-publication-1'});
  expect(await pane.find({type: 'Text', text: 'Shared context detail'})).toBeDefined();
  expect(await pane.find({type: 'Text', text: 'Imported by Agent 2'})).toBeDefined();
  await pane.press({key: 'board-back'});
  await pane.press({key: 'board-details'});
  expect(await pane.find({key: 'detail-checks'})).toBeDefined();
  await pane.press({key: 'detail-checks'});
  expect(await pane.find({type: 'Text', text: 'Recorded pass'})).toBeDefined();
  await pane.unmount();
});

test('narrow native pane keeps both task claims and controls accessible', async ($, on) => {
  on('ui.close', () => ({value: null}));
  on('ui.render', ($, e) => { const {Text} = $.ui.resolve(e); return Text({children: ''}); });
  const pane = await $.ui.mount({plugin: 'delm-board-preview', surface: 'terminal', component: 'Pane', requestId: 'delm',
    props: {title: 'DeLM', isFocused: true, bodyColumns: 86, placement: 'inline', scroll: {offset: 0, bodyRows: 9}, view: {}}});
  expect(await pane.find({key: 'task-2', text: /Claimed · Agent 2/})).toBeDefined();
  expect(await pane.find({key: 'task-3', text: /Claimed · Agent 1/})).toBeDefined();
  expect(await pane.find({key: 'board-hide'})).toBeDefined();
  await pane.press({key: 'board-hide'});
  const compact = await $.ui.mount({plugin: 'delm-board-preview', surface: 'terminal', component: 'AbovePrompt',
    props: {hasSurvey: false, isWorking: false, maxRows: 2, bodyColumns: 85, scroll: {offset: 0, bodyRows: 2}, view: {}}});
  expect(await compact.find({key: 'board-show'})).toBeDefined();
  await compact.redraw({hasSurvey: true, isWorking: false, maxRows: 2, bodyColumns: 85, scroll: {offset: 0, bodyRows: 2}, view: {}});
  expect(await compact.find({key: 'board-show'})).toBeUndefined();
  await compact.unmount();
  await pane.unmount();
});

test('six-row pane exposes task and shared summaries with working native controls', async $ => {
  const pane = await $.ui.mount({plugin: 'delm-board-preview', surface: 'terminal', component: 'Pane', requestId: 'delm',
    props: {title: 'DeLM', isFocused: true, bodyColumns: 76, placement: 'inline', scroll: {offset: 0, bodyRows: 6}, view: {}}});
  expect(await pane.find({key: 'view-all-tasks', text: 'Task queue · 4 tasks · 2 claimed'})).toBeDefined();
  expect(await pane.find({key: 'view-all-shared', text: 'Shared context · 1 shared'})).toBeDefined();
  expect(await pane.find({key: 'board-details'})).toBeDefined();
  expect(await pane.find({key: 'board-hide'})).toBeDefined();
  await pane.press({key: 'view-all-shared'});
  expect(await pane.find({type: 'Text', text: 'Shared context'})).toBeDefined();
  await pane.press({key: 'board-back'});
  await pane.press({key: 'view-all-tasks'});
  expect(await pane.find({type: 'Text', text: 'All tasks'})).toBeDefined();
  await pane.unmount();
});
