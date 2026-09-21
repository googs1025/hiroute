// Shared static-site interactions; the content itself is rendered in each language.
const boundaries = document.documentElement.lang === 'en' ? {
  compaction: {
    kicker: 'A decision point during autonomous work',
    title: 'A rebuilt context is a chance to choose again.',
    copy: 'When ContextHold permits a switch, HiRoute reassesses the work ahead; the client need not send a separate compaction signal.',
    assessment: 'Assess the work already performed by model A',
    choice: 'Choose the primary branch; model B takes over'
  },
  human: {
    kicker: 'A decision point with human input',
    title: 'A new question can also reveal how the last stage went.',
    copy: 'A follow-up question or feedback prompts a new choice and an optional assessment of prior work. New input need not invalidate the cache. If the model remains a good fit, it can continue with the existing prefix.',
    assessment: 'Use the new feedback to assess model B’s stage',
    choice: 'Keep the primary branch; model B continues'
  }
} : {
  compaction: {
    kicker: '自主执行中的决策机会',
    title: '重建上下文时，可以重新选择。',
    copy: 'ContextHold 判断当前边界允许切换时，HiRoute 重新评估下一段工作；不要求客户端额外报告“发生了压缩”。',
    assessment: '回看模型 A 已经完成的执行片段',
    choice: '选择主力分支，让模型 B 接续'
  },
  human: {
    kicker: '用户参与后的决策机会',
    title: '新问题，也是对上一阶段的新线索。',
    copy: '用户补充问题或反馈时，重新判断下一段工作，并可评价此前的表现。新输入不一定破坏已有缓存；如果模型仍胜任，可以保持原模型继续追加上下文。',
    assessment: '结合新反馈，评价模型 B 的阶段表现',
    choice: '示例中仍选主力分支，模型 B 继续'
  }
};

document.querySelectorAll('[data-boundary]').forEach(button => {
  button.addEventListener('click', () => {
    const boundary = boundaries[button.dataset.boundary];
    document.querySelectorAll('[data-boundary]').forEach(item => {
      item.setAttribute('aria-pressed', String(item === button));
    });
    for (const field of ['kicker', 'title', 'copy', 'assessment', 'choice']) {
      document.getElementById(`boundary-${field}`).textContent = boundary[field];
    }
  });
});

// FAQ links open their answer. Native links/details also work without JavaScript.
function revealLinkedAnswer() {
  const target = document.getElementById(location.hash.slice(1));
  if (target instanceof HTMLDetailsElement) target.open = true;
}
window.addEventListener('hashchange', revealLinkedAnswer);
document.querySelectorAll('a[href^="#"]').forEach(link => {
  link.addEventListener('click', () => {
    const target = document.getElementById(link.getAttribute('href').slice(1));
    if (target instanceof HTMLDetailsElement) target.open = true;
  });
});
document.querySelectorAll('[data-language]').forEach(link => {
  link.addEventListener('click', () => {
    const target = new URL(link.href);
    target.hash = location.hash;
    link.href = target.href;
  });
});
revealLinkedAnswer();
