const form = document.querySelector('#add-task');
const title = document.querySelector('#title');
const list = document.querySelector('#tasks');
let tasks = [];

function render() {
  list.replaceChildren();
  for (const task of tasks) {
    const row = document.createElement('li');
    row.dataset.complete = String(task.complete);
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.checked = task.complete;
    checkbox.id = `task-${task.id}`;
    checkbox.addEventListener('change', () => {
      tasks = TaskModel.toggleTask(tasks, task.id);
      render();
    });
    const label = document.createElement('label');
    label.htmlFor = checkbox.id;
    label.textContent = task.title;
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.textContent = 'Remove';
    remove.setAttribute('aria-label', `Remove ${task.title}`);
    remove.addEventListener('click', () => {
      tasks = TaskModel.removeTask(tasks, task.id);
      render();
    });
    row.append(checkbox, label, remove);
    list.append(row);
  }
}

form.addEventListener('submit', event => {
  event.preventDefault();
  if (!title.value.trim()) return;
  tasks = [...tasks, TaskModel.createTask(crypto.randomUUID(), title.value)];
  title.value = '';
  render();
  title.focus();
});
render();
