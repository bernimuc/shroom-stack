# Ideas for new features

## Supprt for auto rotate

The application should support to figure out the correct rotation of the pictures
and use it to display everything in the correct orientation by default, allowing 
user to override that in case automatic orientation is not working for a specific 
case (which sometimes happens when taking pictures at strange angles).

## Camera RAW Support

Right now we rely on Adobes DNG exporter to get the picture data. It would
be better when the application could directly read raw files from different 
manufacturers.

## Better lightroom integration

A lightroom plugin that can be used via the export menü would be nice to have.
The plugin should enable the user to select the files for a stack, use the 
export menu to transfer those files into shroom stack and when the shroom
stack application saves the file, automatically imports the resulting picture.

## Use generative AI to fill missing information

One inherent problem with focus stacking is when foreground objects overlap 
background that is supposed to be sharp in one of the pictures. Because the
foreground is rendered very out of focus in the pictures that have the detail
for the background, some picture information in the transition zone between
foreground and background is missing.

A nice feature would be automatically identifying such problematic zones, 
generating the missing information with generative AI and then blending
the generated image in on the parts where it's necessary.

If a fully automatic identification of problematic zones is not possible,
the next best option would be to rely on user input, e.g. allow the user 
to paint a mask to indicate parts of the picture where additional detail is 
expected and then automatically refining that selection by removing those parts
that already have enough detail.

## Soft-Bokeh

The whole point of focus stacking is identifying the pictures with the most 
detail and using that in the output picture. 

For areas that are supposed to be out of focus, like the background bokeh, this 
is not always the most desirable option. I often choose a large aperture when 
creating focus stacks, so that the background is nicely out of focus. When the 
stacking software then chooses the frame with the furthest focus plane for those
areas, this kind of defeats the creative purpose of choosing a large aperture.

So it would be nice to automatically recognize parts of the picture that are very 
out of focus and then choosing the image with the nearest focus plane (or the most
"out of focus" image alternatively) for those parts. 

Obviously this is a special case that should only be enabled when the user
wants this, like with a checkbox "soft bokeh".

## Stack management 

It would be nice to have some tools managing the stacks. We often take more than one stack
in a session and all the pictures land in a single folder.

The app could automatically recognize, which photos belong to which stack and create 
groups of pictures accordingly. This could be done in a new workspace.

Also it would be great to transfer this information to lightroom. I personally like to assign
pictures that belong to a stack the same color marker. Different stacks get different colors.
Maybe it would be possible to write this information to the XMP files and have lightroom 
update accordingly.