let a=[];
for(let i=0;i<2000;i++) a.push((i*7919)%2000);
a.sort((x,y)=>x-y);
console.log(a[0], a[1000], a[1999]);
