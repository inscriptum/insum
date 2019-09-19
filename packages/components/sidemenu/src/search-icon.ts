import { Define, AbstractElement } from 'abstract-element';
import litRender from 'abstract-element/render/lit';
import { html } from 'lit-html';

@Define('insum-search-icon')
export class SearchIcon extends AbstractElement {
  color = '#d9d9d9';

  constructor() {
    super(litRender, false);
  }

  render() {
    return html`
      <svg width="18" height="40" viewBox="0 0 14 14" fill="none" xmlns="http://www.w3.org/2000/svg">
        <g clip-path="url(#clip0)">
          <path
            d="M5.13158 9.76316C7.68953 9.76316 9.76316 7.68953 9.76316 5.13158C9.76316 2.57363 7.68953 0.5 5.13158 0.5C2.57363 0.5 0.5 2.57363 0.5 5.13158C0.5 7.68953 2.57363 9.76316 5.13158 9.76316Z"
            stroke=${this.color}
          />
          <path d="M5.13157 2.36841C3.60552 2.36841 2.36841 3.60552 2.36841 5.13157" stroke=${this.color} stroke-linecap="round" />
          <path
            d="M9.45134 9.45143C9.71854 9.18423 9.71854 8.75101 9.45134 8.48381C9.18414 8.21661 8.75092 8.21661 8.48372 8.48381C8.21652 8.75101 8.21652 9.18423 8.48372 9.45143C8.75092 9.71863 9.18414 9.71863 9.45134 9.45143Z"
            fill="white"
            stroke=${this.color}
          />
          <path
            d="M13.0757 12.1081L10.8427 9.87511C10.5755 9.6079 10.1423 9.60791 9.87508 9.87511C9.60788 10.1423 9.60788 10.5755 9.87508 10.8427L12.108 13.0757C12.3752 13.3429 12.8085 13.3429 13.0757 13.0757C13.3429 12.8085 13.3429 12.3753 13.0757 12.1081Z"
            fill="white"
            stroke=${this.color}
          />
        </g>
        <defs>
          <clipPath id="clip0">
            <rect width="14" height="14" fill="white" />
          </clipPath>
        </defs>
      </svg>
    `;
  }
}
