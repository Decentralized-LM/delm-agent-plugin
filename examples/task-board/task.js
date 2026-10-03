(function (root) {
  const model = {
    createTask(id, title) {
      const trimmed = title.trim();
      if (!trimmed) throw new Error('A task needs a title.');
      return { id, title: trimmed, complete: false };
    },
    toggleTask(tasks, id) {
      return tasks.map(task => task.id === id ? { ...task, complete: !task.complete } : task);
    },
    removeTask(tasks, id) {
      return tasks.filter(task => task.id !== id);
    }
  };
  if (typeof module !== 'undefined') module.exports = model;
  else root.TaskModel = model;
})(globalThis);
