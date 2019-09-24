import { SideMenu } from '../src';

// create a component
SideMenu;
const demoMenu: SideMenu = document.querySelector('insum-sidemenu');

const content = document.querySelector('#content');

demoMenu.addEventListener('changeActive', () => {
  console.log(demoMenu.activeMenuItem);
  switch (demoMenu.activeMenuItem) {
    case 'grid':
      content.innerHTML = '<p>grid start</p><p>----content----</p><p>grid stop</p>';
      break;
    case 'style':
      content.innerHTML = '<p>style start</p><p>----content----</p><p>headers style</p>';
      break;

    default:
      content.innerHTML = '';
      break;
  }
});

// add data obj to the menu component
demoMenu.data = {
  component: {
    label: 'Компонент',
    category: true,
  },
  style: {
    label: 'Стили',
    available: true,
    category: true,
  },
  tooltip: {
    label: 'Tooltip',
    parent: 'component',
    available: true,
  },
  slider1: {
    label: 'Slider1',
    parent: 'slider',
    available: true,
    hide: true,
  },
  slider_hide: {
    label: 'Slider hide',
    parent: 'slider',
    available: true,
    hide: true,
  },
  slider: {
    label: 'Slider',
    parent: 'component',
  },
  grid: {
    label: 'Grid',
    parent: 'style',
    available: true,
  },
  grid1: {
    label: 'Grid1',
    parent: 'grid',
    available: true,
  },
};
