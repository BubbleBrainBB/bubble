import assert from 'node:assert/strict';
import vm from 'node:vm';
import ts from 'typescript';
import { getBrowserUtilitySessionId } from '../../src/ui/utils/browser-utility';
import { isRightUtilityBrowserTab } from '../../src/ui/utils/right-utility-tabs';
import { readFile } from 'node:fs/promises';
import type { SessionRightPanelLiveFields, SessionRightPanelSnapshot } from '../../src/ui/types';
import {
  captureLiveRightPanel,
  emptyRightPanelSnapshot,
  liveFieldsFromRightPanel,
  liveRightPanelEquals,
  migrateRightPanelSessionId,
  persistRightPanelBySessionId,
  pruneRightPanelBySessionId,
  rightPanelSessionKey,
  switchSessionRightPanel,
  withFileTabsForUtilityTab,
} from '../../src/ui/utils/session-right-panel';

function live(partial: Partial<SessionRightPanelLiveFields> = {}): SessionRightPanelLiveFields {
  return {
    rightUtilityTabs: [],
    activeRightUtilityTab: null,
    rightUtilityPanelHidden: false,
    rightPanelFullscreen: null,
    projectTreeCollapsed: true,
    projectPanelView: 'files',
    browserPanelOpen: false,
    reviewDiffSelection: null,
    ...partial,
  };
}

function snapshot(partial: Partial<SessionRightPanelSnapshot> = {}): SessionRightPanelSnapshot {
  const next = {
    ...emptyRightPanelSnapshot(),
    ...partial,
  };
  if (partial.hidden === undefined && next.tabs.length > 0) {
    next.hidden = false;
  }
  return next;
}

function testSameCwdSessionsKeepIndependentPanels() {
  const sessions = { 'session-a': {}, 'session-b': {} };
  const sessionA = live({
    rightUtilityTabs: ['files:aaa', 'browser'],
    activeRightUtilityTab: 'files:aaa',
    projectTreeCollapsed: false,
  });
  const map = withFileTabsForUtilityTab(
    {
      [rightPanelSessionKey('session-a')]: snapshot({
        tabs: ['files:aaa', 'browser'],
        activeTab: 'files:aaa',
      }),
    },
    'session-a',
    'files:aaa',
    {
      files: [{ cwd: '/shared', filePath: '/shared/a.ts', name: 'a.ts', viewMode: 'code' }],
      activeFile: { cwd: '/shared', filePath: '/shared/a.ts' },
    },
    sessionA
  );

  const switchedToB = switchSessionRightPanel({
    prevSessionId: 'session-a',
    nextSessionId: 'session-b',
    live: sessionA,
    rightPanelBySessionId: map,
    sessions,
  });

  assert.deepEqual(switchedToB.rightUtilityTabs, [], 'session B must start with its own empty panel');
  assert.equal(switchedToB.activeRightUtilityTab, null);
  assert.equal(
    switchedToB.rightUtilityPanelHidden,
    true,
    'a session that never opened the right panel must not expand it'
  );
  assert.equal(switchedToB.rightPanelFullscreen, null);
  assert.equal(switchedToB.browserPanelOpen, false);
  assert.equal(
    switchedToB.rightPanelBySessionId['session-a']?.fileTabsByUtilityTab['files:aaa']?.files[0]?.filePath,
    '/shared/a.ts',
    'session A file tabs are stored, not shared via cwd'
  );

  const sessionB = live({
    rightUtilityTabs: ['browser'],
    activeRightUtilityTab: 'browser',
    browserPanelOpen: true,
  });
  const afterB = switchSessionRightPanel({
    prevSessionId: 'session-b',
    nextSessionId: 'session-a',
    live: sessionB,
    rightPanelBySessionId: {
      ...switchedToB.rightPanelBySessionId,
      'session-b': captureLiveRightPanel(sessionB, switchedToB.rightPanelBySessionId, 'session-b'),
    },
    sessions,
  });

  assert.deepEqual(afterB.rightUtilityTabs, ['files:aaa', 'browser']);
  assert.equal(afterB.activeRightUtilityTab, 'files:aaa');
  assert.equal(
    afterB.rightPanelBySessionId['session-a']?.fileTabsByUtilityTab['files:aaa']?.files[0]?.filePath,
    '/shared/a.ts'
  );
  assert.deepEqual(afterB.rightPanelBySessionId['session-b']?.tabs, ['browser']);
}

function testPersistStripsEphemeralTabs() {
  const persisted = persistRightPanelBySessionId({
    'session-a': snapshot({
      tabs: ['files:aaa', 'browser', 'side-chat:fork-1', 'subagent:tool-1'],
      activeTab: 'side-chat:fork-1',
      fileTabsByUtilityTab: {
        'files:aaa': {
          files: [{ cwd: '/shared', filePath: '/shared/a.ts' }],
          activeFile: { cwd: '/shared', filePath: '/shared/a.ts' },
        },
      },
    }),
  });

  assert.deepEqual(persisted['session-a']?.tabs, ['files:aaa', 'browser']);
  assert.equal(persisted['session-a']?.activeTab, 'files:aaa');
  assert.equal(persisted['session-a']?.reviewDiffSelection, null);
  assert.equal(
    persisted['session-a']?.fileTabsByUtilityTab['files:aaa']?.files[0]?.filePath,
    '/shared/a.ts'
  );
}

function testMigrateDraftKeepsOpenFiles() {
  const draftId = 'draft-1';
  const realId = 'session-real';
  const map = withFileTabsForUtilityTab(
    {
      [draftId]: snapshot({
        tabs: ['files:draft'],
        activeTab: 'files:draft',
      }),
    },
    draftId,
    'files:draft',
    {
      files: [{ cwd: '/shared', filePath: '/shared/notes.md' }],
      activeFile: { cwd: '/shared', filePath: '/shared/notes.md' },
    }
  );
  const migrated = migrateRightPanelSessionId(map, draftId, realId);
  assert.equal(migrated[draftId], undefined);
  assert.deepEqual(migrated[realId]?.tabs, ['files:draft']);
  assert.equal(
    migrated[realId]?.fileTabsByUtilityTab['files:draft']?.files[0]?.filePath,
    '/shared/notes.md'
  );
}

function testDeletedSessionIsPruned() {
  const map = {
    'session-a': snapshot({ tabs: ['browser'], activeTab: 'browser' }),
    'session-b': snapshot({ tabs: ['files:bbb'], activeTab: 'files:bbb' }),
  };
  const pruned = pruneRightPanelBySessionId(map, ['session-b']);
  assert.equal(pruned['session-a'], undefined);
  assert.ok(pruned['session-b']);
}

function testLiveFieldsFollowActiveTab() {
  const fields = liveFieldsFromRightPanel(
    snapshot({
      tabs: ['files:aaa', 'browser'],
      activeTab: 'browser',
    })
  );
  assert.equal(fields.browserPanelOpen, true);
  assert.equal(fields.projectTreeCollapsed, true);
  assert.equal(fields.rightUtilityPanelHidden, false);
}

function testEmptySessionKeepsPanelCollapsed() {
  const fields = liveFieldsFromRightPanel(emptyRightPanelSnapshot());
  assert.deepEqual(fields.rightUtilityTabs, []);
  assert.equal(fields.activeRightUtilityTab, null);
  assert.equal(fields.rightUtilityPanelHidden, true);
  assert.equal(fields.rightPanelFullscreen, null);
  assert.equal(fields.browserPanelOpen, false);
}

function testFullViewSurvivesSessionRoundTrip() {
  for (const [tab, kind] of [['files:doc', 'files'], ['browser', 'browser'], ['review', 'review']] as const) {
    const sessions = { a: {}, b: {} };
    const away = switchSessionRightPanel({
      prevSessionId: 'a',
      nextSessionId: 'b',
      live: live({ rightUtilityTabs: [tab], activeRightUtilityTab: tab, rightPanelFullscreen: kind }),
      rightPanelBySessionId: {},
      sessions,
    });
    const back = switchSessionRightPanel({
      prevSessionId: 'b',
      nextSessionId: 'a',
      live: away,
      rightPanelBySessionId: away.rightPanelBySessionId,
      sessions,
    });
    assert.equal(back.rightPanelFullscreen, kind, `${kind} full view must survive leaving and re-entering a session`);
  }
}

function main() {
  testFullViewSurvivesSessionRoundTrip();
  const design = persistRightPanelBySessionId({ a: snapshot({ tabs: ['design', 'design:canvas-a'], activeTab: 'design:canvas-a', fullscreen: 'design' }) });
  assert.deepEqual(design.a.tabs, ['design', 'design:canvas-a']);
  assert.equal(design.a.activeTab, 'design:canvas-a');
  assert.equal(liveFieldsFromRightPanel(design.a).rightPanelFullscreen, 'design');
  testSameCwdSessionsKeepIndependentPanels();
  testPersistStripsEphemeralTabs();
  testMigrateDraftKeepsOpenFiles();
  testDeletedSessionIsPruned();
  testLiveFieldsFollowActiveTab();
  testEmptySessionKeepsPanelCollapsed();
  void assertWiring();
  void testDraftPanelIsolation();
  void testNativeHideBeforeSessionPanelRestore();
}

async function assertWiring() {
  const [appSource, storeSource, treePanelSource] = await Promise.all([
    readFile(new URL('../../src/ui/App.tsx', import.meta.url), 'utf8'),
    readFile(new URL('../../src/ui/store/useAppStore.ts', import.meta.url), 'utf8'),
    readFile(new URL('../../src/ui/components/ProjectTreePanel.tsx', import.meta.url), 'utf8'),
  ]);
  assert.match(
    appSource,
    /key=\{\`\$\{activeSessionId \?\? 'new'\}:\$\{tabId\}\`\}/,
    'Files and Browser panels must remount per session so the same cwd cannot share React state'
  );
  assert.match(storeSource, /switchSessionRightPanel\(/);
  assert.match(storeSource, /rightPanelBySessionId:/);
  assert.match(
    appSource,
    /setRightPanelLauncherOpen\(false\);\s*\}, \[activeSessionId\]\)/,
    'switching sessions must dismiss the right-panel launcher so an empty session stays collapsed'
  );
  assert.match(treePanelSource, /syncSessionFileTabs\(/);
  assert.match(
    treePanelSource,
    /if \(isFullscreen && fileTabsHydrated && !selectedFilePath\)/,
    'a remounted Files panel must restore its document before deciding full view has nothing to show'
  );
  assert.match(
    treePanelSource,
    /prevTreeResetCwdRef[\s\S]{0,400}setExpandedPaths\(new Set\(\)\)/,
    'cwd changes may reset tree chrome but must not be the old wipe-all-files effect'
  );
  assert.doesNotMatch(
    treePanelSource,
    /if \(openRequest && openRequest\.cwd === cwd\) return;[\s\S]{0,400}updateOpenFileTabs\(\(\) => \[\]\)/,
    'Files panels must not wipe open tabs just because cwd is shared across sessions'
  );
  console.log('session-right-panel tests passed');
}

void main();


async function testDraftPanelIsolation() {
  const source = await readFile(new URL('../../src/ui/store/useAppStore.ts', import.meta.url), 'utf8');
  const tree = ts.createSourceFile('store.ts', source, ts.ScriptTarget.Latest, true);
  let action: ts.Expression | undefined;
  function visit(node: ts.Node) {
    if (ts.isPropertyAssignment(node) && node.name.getText(tree) === 'createDraftSession') action = node.initializer;
    ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.ok(action, 'exercise the real createDraftSession action');
  for (const previousSession of ['session-a', null]) {
    let state: any = {
      ...live({ rightUtilityTabs: ['browser'], activeRightUtilityTab: 'browser', browserPanelOpen: true }),
      activeSessionId: previousSession,
      projectCwd: '/shared', activeChannelByProject: {}, sessions: {},
      rightPanelBySessionId: {}, workspaceLayout: {},
    };
    const context = vm.createContext({
      get: () => state,
      set: (update: any) => { state = { ...state, ...update(state) }; },
      createDraftSessionView: () => ({ id: 'new-draft', isDraft: true }),
      resolveActiveChannelIdForProject: () => 'default',
      getProjectChannelKey: (cwd: string) => cwd,
      normalizeWorkspaceChannelId: (id: string) => id,
      captureLiveRightPanel, emptyRightPanelSnapshot,
      pickLiveRightPanel: (value: any) => value,
      tree: { getActiveLeaf: () => ({ id: 'main' }), placeSession: (_layout: any, _leaf: string, id: string) => ({ id }) },
      layoutPatch: (layout: any) => ({ activeSessionId: layout.id }),
      persistUiResumeStateSnapshot: () => {},
    });
    vm.runInContext(ts.transpileModule(`globalThis.createDraft = (${action.getText(tree)});`, {
      compilerOptions: { target: ts.ScriptTarget.ES2022 },
    }).outputText, context);
    context.createDraft('/shared');
    const panel = state.rightPanelBySessionId['new-draft'];
    assert.deepEqual(panel.tabs, previousSession ? [] : ['browser']);
    assert.equal(panel.hidden, previousSession !== null);
  }
  console.log('draft panel isolation passed: new conversations start empty; standalone promotion survives');
}


async function testNativeHideBeforeSessionPanelRestore() {
  const source = await readFile(new URL('../../src/ui/store/useAppStore.ts', import.meta.url), 'utf8');
  const tree = ts.createSourceFile('store.ts', source, ts.ScriptTarget.Latest, true);
  let watcher: ts.Expression | undefined;
  function visit(node: ts.Node) {
    if (ts.isCallExpression(node) && node.expression.getText(tree) === 'useAppStore.subscribe'
      && node.arguments[0]?.getText(tree).includes('switchSessionRightPanel')) watcher = node.arguments[0];
    ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.ok(watcher);
  const events: unknown[] = [];
  const prev = { ...live({ rightUtilityTabs: ['browser:extra'], activeRightUtilityTab: 'browser:extra', browserPanelOpen: true }),
    activeSessionId: 'a', sessions: { a: {}, b: {} }, rightPanelBySessionId: {} };
  const context = vm.createContext({
    window: { electron: { browser: { hide: (input: any) => { events.push(['hide', input.sessionId]); return Promise.resolve(); } } } },
    getBrowserUtilitySessionId, isRightUtilityBrowserTab, switchSessionRightPanel, liveRightPanelEquals,
    pickLiveRightPanel: (value: any) => value,
    useAppStore: { setState: (patch: any) => { events.push(['restore', patch.browserPanelOpen, patch.rightUtilityPanelHidden]); } },
  });
  vm.runInContext(ts.transpileModule(`globalThis.watch = (${watcher.getText(tree)});`, {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText, context);
  context.watch({ ...prev, activeSessionId: 'b' }, prev);
  assert.deepEqual(events, [['hide', 'a:browser:extra'], ['restore', false, true]]);
  console.log('native browser hide is dispatched before restoring the next session panel');
}
