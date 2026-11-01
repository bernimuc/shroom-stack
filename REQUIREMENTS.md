# Shroom Stack

## Focus Stacking application 

This application can be used to load 2 or more photos with different focal planes 
and combine those into a single picture where the most in focus parts of each 
picture is used.

Two modes of combining the pictures should be supported:

1) Depth Map: the application creates a depth map by selecting the sharpest pixels 
   of the input pictures and uses this depth map to create the output image
2) Pyamid: uses a pyramid based approach to divide the image information into
   high- and low-frequency components. Combines those components into the output 
   image

Input- and output format should be DNG only for now. At a later stage we might 
add additional formats (JPG, TIFF, Camera RAW Files like CR3).

## Workflow and user interface

The application has different workspaces. Until a stack starts to be processed,
the workspace "Preparation" is shown and other workspaces cannot be accessed.

The application should have the following workspaces:

### Workspace 1: "Preparation" 

This is the initial workspace that is displayed when the user starts the
application. Here the user can select the files he wants to process, choose the
rendering method and applicable parameters.

The UI should consist of the following elements:

1) A file list on the left side. Here the user can see the currently selected 
   files with a thumbnail and metadata (Filename, file date).
2) Preview-Window in the middle. Here the user can see a larger preview of 
   the file that is selected in the file list on the left side
3) Toolbar on top. Here the user can select the rendering method and any parameters
   that might be applicable to that rendering method. This toolbar also contains
   a "render" button that starts the processing.

### Workspace 2: "Processing"

As soon as the user clicks "render" the program checks if all preconditions for 
rendering are met. At least 2 image files should be loaded. Any additional checks
that determine if the images can be stacked successfully can be made in this step. 

If everything is all right, the app switches to workspace "Processing".

This workspace shows the following components:

1) The same file list like in "Preparation" on the left side. The image that the
   app is currently worked on should be selected or marked somehow.
2) A preview in the center part that is split in the middle. Left showing the
   image that is worked on right now, right showing a visualization of the data
   that is created, like the created depth map.

As soon as the stacking is done, the next workspace displaying the output is shown.

### Workspace 3: "Postprocessing"

This workspace is used to display the generated image, fine tune it and also safe it.

It consists of the following UI elements:

1) Again, the file list with the input files as in other workspaces.
2) A split view similar to "Processing" in the center, that displays the via file list 
   selected image on the left and the output image on the right.
3) A toolbar with the following functionality:
    2) Brush-Button. The user should be able to use a brush to fine tune the result by
       selecting one of the source images in the file list on the left side and then using
       the brush to paint in parts of the selected input image into the result image.
    1) Save-Button. This opens a "save as"-dialog that allows the user to select 
       a file format (for now DNG only). This should preferrably be a OS native dialog.


### Common functionality to the preview-window

All workspaces contain some kind of preview window that show either a single picture
or a split view.

Initially this windows should display the image scaled to fit. But the user can
zoom in and out and pan around using the following controls:

1) Mousewheel zooms in / out
2) SPACE-Drag pans the image
3) Below the preview window or layered on top should be some small controls 
   with the same functionality: 
   1) Label to show current zoom level in %
   2) Icon to switch to 1:1 view
   3) Icon to zoom in (+)
   4) Icon to zoom out (-)
   5) Slider for Zoom (min is fit whole image, max is 1:1)

The left and right view always should be in sync, so we only need one set of
controls, not one per view.

## Validation and examples

I have provided some example stacks in the folder "examples". 

The files with the 8 character names are the files from the camera exported via 
lightroom as dng.

The other two are files processed by a similar software (helicon focus). The first
character in brackets indicates the method used, "B" is depth map, "C" is pyramid.  

Note: because of its size we should probably not put this folder in git.

## Open questions

1) Would it be sensible to align the pictures first, in case there was
   slight movements of the camera or the subject between frames?
2) Is the user responsible to specify the correct order of input files? Can this
   be determined automatically?

## Technical considerations

The application should be written in rust. 

All the core functionality like image processing and calculations should be 
a separate module. The UI should have its own module. There should be a clear 
interface between both. 

Very processing intensive calculations should be done via GPU. This application
should be able to process larger image stacks with acceptable performance.

Everything should have appropriate unit tests.


