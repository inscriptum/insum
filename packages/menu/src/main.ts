import { MenuComponent } from './menu.component';


// create MenuComponent
MenuComponent;
const _demoMenu: MenuComponent = document.querySelector('insum-menu');


// add data obj to the menu component
_demoMenu.data = [
  {
    title: 'Стили',
    hide: false,
    links: [
      {
        id: 'grid',
        title: 'Grid'
      },
      {
        id: 'headers',
        title: 'Headers'
      },
      {
        id: 'input',
        title: 'Input'
      },
      {
        id: 'input-group',
        title: 'Input Group'
      },
      {
        id: 'layout-form',
        title: 'Layout Form'
      },
      {
        id: 'button-group',
        title: 'Button Group'
      },

    ],
  }, {
    title: 'Веб-компоненты',
    hide: true,
    links: [
      {
        id: 'spinner',
        title: 'Spinner'
      },
      {
        id: 'slider',
        title: 'Slider'
      },
    ],
  },
];
