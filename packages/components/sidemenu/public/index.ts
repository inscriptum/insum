import { SideMenu } from '../src';

// create a component
SideMenu;
const demoMenu: SideMenu = document.querySelector('insum-sidemenu');

const content = document.querySelector('#content');

demoMenu.addEventListener('change-active', () => {
  console.log(demoMenu.activeMenuItem);
  switch (demoMenu.activeMenuItem) {
    case 'grid':
      content.innerHTML = '<p>grid start</p><p>----content----</p><p>grid stop</p>';
      break;
    case 'headers':
      content.innerHTML = '<p>headers start</p><p>----content----</p><p>headers stop</p>';
      break;

    default:
      content.innerHTML = '';
      break;
  }
});

// add data obj to the menu component
demoMenu.data = [
  {
    title: 'Стили',
    hide: false,
    links: [
      {
        id: 'grid',
        title: 'Grid',
      },
      {
        id: 'headers',
        title: 'Headers',
      },
      {
        id: 'input',
        title: 'Input',
      },
      {
        id: 'input-group',
        title: 'Input Group',
      },
      {
        id: 'layout-form',
        title: 'Layout Form',
      },
      {
        id: 'button-group',
        title: 'Button Group',
      },
    ],
  },
  {
    title: 'Веб-компоненты',
    hide: true,
    links: [
      {
        id: 'spinner',
        title: 'Spinner',
      },
      {
        id: 'slider',
        title: 'Slider',
      },
    ],
  },
];
